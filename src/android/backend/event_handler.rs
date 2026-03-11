use crate::android::backend::{CentralizedEvent, WaylandBackend};
use crate::android::compositor::{send_frames_surface_tree, ClientState, State};
use crate::android::window_manager::WindowEvent;
use smithay::backend::renderer::element::surface::{
    render_elements_from_surface_tree, WaylandSurfaceRenderElement,
};
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::utils::draw_render_elements;
use smithay::backend::renderer::{Color32F, Frame, Renderer};
use smithay::input::keyboard::FilterResult;
use smithay::input::{pointer, touch};
use smithay::reexports::wayland_server::protocol::wl_pointer::ButtonState;
use smithay::utils::{Rectangle, Transform, SERIAL_COUNTER};
use smithay::wayland::shell::xdg::ToplevelSurface;
use smithay::{
    backend::input::{
        AbsolutePositionEvent, Axis, Event, InputEvent, KeyboardKeyEvent, PointerAxisEvent,
        PointerButtonEvent, TouchEvent,
    },
    output::{Mode, Scale},
};
use std::sync::Arc;
use winit::event_loop::ActiveEventLoop;

/// Find the toplevel surface for a given window_id from the window manager.
fn get_surface_for_window(backend: &WaylandBackend, window_id: u32) -> Option<ToplevelSurface> {
    backend
        .window_manager
        .as_ref()
        .and_then(|wm| wm.windows.get(&window_id))
        .map(|w| w.toplevel.clone())
}

/// Get the first toplevel (fallback for main window / single-window mode).
fn get_first_surface(state: &State) -> Option<ToplevelSurface> {
    state
        .xdg_shell_state
        .toplevel_surfaces()
        .iter()
        .next()
        .cloned()
}

pub fn handle(
    event: CentralizedEvent,
    backend: &mut WaylandBackend,
    event_loop: &ActiveEventLoop,
) {
    match event {
        CentralizedEvent::CloseRequested => {
            event_loop.exit();
        }
        CentralizedEvent::Redraw => {
            // --- 1. Process pending toplevels: create Activity windows ---
            let pending: Vec<ToplevelSurface> =
                backend.compositor.state.pending_toplevels.drain(..).collect();
            if !pending.is_empty() {
                if let Some(wm) = backend.window_manager.as_mut() {
                    for toplevel in pending {
                        wm.new_toplevel(toplevel);
                    }
                }
            }

            // --- 2. Process window events from JNI ---
            process_window_events(backend);

            // --- 3. Accept Wayland clients, dispatch protocol ---
            {
                let compositor = &mut backend.compositor;
                if let Some(stream) = compositor
                    .listener
                    .accept()
                    .expect("Failed to accept listener")
                {
                    let client = compositor
                        .display
                        .handle()
                        .insert_client(stream, Arc::new(ClientState::default()))
                        .unwrap();
                    compositor.clients.push(client);
                }

                compositor
                    .display
                    .dispatch_clients(&mut compositor.state)
                    .expect("Failed to dispatch clients");
                compositor
                    .display
                    .flush_clients()
                    .expect("Failed to flush clients");
            }

            // --- 4. Render each Activity window ---
            render_activity_windows(backend);

            // --- 5. Render main window (background) ---
            render_main_window(backend);

            // Request next frame
            if let Some(winit) = backend.graphic_renderer.as_ref() {
                winit.window().request_redraw();
            }
        }
        CentralizedEvent::Input(event) => {
            // Input from the main NativeActivity window (winit).
            // Route to first toplevel as fallback.
            handle_winit_input(event, backend);
        }
        CentralizedEvent::Resized { size, scale_factor } => {
            if let Some(output) = &backend.compositor.output {
                output.change_current_state(
                    Some(Mode {
                        size: size.into(),
                        refresh: 60000,
                    }),
                    Some(Transform::Normal),
                    Some(Scale::Fractional(scale_factor)),
                    Some((0, 0).into()),
                );
            }
        }
        _ => (),
    }
}

/// Process events from WaylandWindowActivity JNI callbacks.
fn process_window_events(backend: &mut WaylandBackend) {
    // Collect events first to avoid borrow issues
    let events: Vec<WindowEvent> = backend
        .window_manager
        .as_ref()
        .map(|wm| wm.event_rx.try_iter().collect())
        .unwrap_or_default();

    for event in events {
        match event {
            WindowEvent::SurfaceCreated {
                window_id,
                native_window,
            } => {
                // Create EGL surface for this Activity window
                if let (Some(wm), Some(winit)) = (
                    backend.window_manager.as_mut(),
                    backend.graphic_renderer.as_ref(),
                ) {
                    if let Some(window) = wm.windows.get_mut(&window_id) {
                        window.native_window = Some(native_window);
                        if let Some(handle) = wm.get_native_handle(window_id) {
                            match winit.create_surface_for_native_window(handle) {
                                Ok(surface) => {
                                    log::info!(
                                        "Created EGL surface for window_id={}",
                                        window_id
                                    );
                                    // Re-borrow to set the surface
                                    if let Some(window) =
                                        backend.window_manager.as_mut().unwrap().windows.get_mut(&window_id)
                                    {
                                        window.egl_surface = Some(surface);
                                    }
                                }
                                Err(e) => {
                                    log::error!(
                                        "Failed to create EGL surface for window_id={}: {:?}",
                                        window_id,
                                        e
                                    );
                                }
                            }
                        }
                    }
                }
            }
            WindowEvent::SurfaceChanged {
                window_id,
                width,
                height,
            } => {
                if let Some(wm) = backend.window_manager.as_mut() {
                    if let Some(window) = wm.windows.get_mut(&window_id) {
                        window.size = (width, height).into();
                        window.needs_redraw = true;

                        // Send configure to the Wayland client with new size
                        window
                            .toplevel
                            .with_pending_state(|state| {
                                state.size = Some((width, height).into());
                            });
                        window.toplevel.send_configure();
                        log::info!(
                            "Window {} resized to {}x{}",
                            window_id,
                            width,
                            height
                        );
                    }
                }
            }
            WindowEvent::SurfaceDestroyed { window_id } => {
                if let Some(wm) = backend.window_manager.as_mut() {
                    if let Some(window) = wm.windows.get_mut(&window_id) {
                        window.egl_surface = None;
                        window.native_window = None;
                        log::info!("Surface destroyed for window_id={}", window_id);
                    }
                }
            }
            WindowEvent::WindowClosed { window_id } => {
                if let Some(wm) = backend.window_manager.as_mut() {
                    if let Some(window) = wm.windows.get(&window_id) {
                        // Tell the Wayland client to close
                        window.toplevel.send_close();
                    }
                    wm.remove_window(window_id);
                }
            }
            WindowEvent::Touch {
                window_id,
                action,
                x,
                y,
            } => {
                handle_activity_touch(backend, window_id, action, x, y);
            }
            WindowEvent::Key {
                window_id,
                key_code,
                action,
                meta_state: _,
            } => {
                handle_activity_key(backend, window_id, key_code, action);
            }
        }
    }
}

/// Render each Activity window's toplevel to its EGL surface.
fn render_activity_windows(backend: &mut WaylandBackend) {
    // Collect window IDs that have an EGL surface ready
    let window_ids: Vec<u32> = backend
        .window_manager
        .as_ref()
        .map(|wm| {
            wm.windows
                .iter()
                .filter(|(_, w)| w.egl_surface.is_some() && w.size.w > 0 && w.size.h > 0)
                .map(|(id, _)| *id)
                .collect()
        })
        .unwrap_or_default();

    for window_id in window_ids {
        // Split borrows: get mutable refs to both graphic_renderer and window_manager
        let (winit, wm) = match (
            backend.graphic_renderer.as_mut(),
            backend.window_manager.as_mut(),
        ) {
            (Some(winit), Some(wm)) => (winit, wm),
            _ => continue,
        };

        let window = match wm.windows.get_mut(&window_id) {
            Some(w) => w,
            None => continue,
        };

        let egl_surface = match window.egl_surface.as_mut() {
            Some(s) => s,
            None => continue,
        };

        let size = window.size;
        let damage = Rectangle::from_size(size);
        let toplevel = window.toplevel.clone();

        // Bind renderer to this window's EGL surface
        let (renderer, mut framebuffer) = match winit.bind_surface(egl_surface) {
            Ok(r) => r,
            Err(e) => {
                log::error!("Failed to bind surface for window_id={}: {:?}", window_id, e);
                continue;
            }
        };

        // Render this toplevel
        let elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
            render_elements_from_surface_tree(
                renderer,
                toplevel.wl_surface(),
                (0, 0),
                1.0,
                1.0,
                Kind::Unspecified,
            );

        let mut frame = match renderer.render(&mut framebuffer, size, Transform::Flipped180) {
            Ok(f) => f,
            Err(e) => {
                log::error!("Failed to begin render for window_id={}: {:?}", window_id, e);
                continue;
            }
        };

        let _ = frame.clear(Color32F::new(0.0, 0.0, 0.0, 1.0), &[damage]);
        let _ = draw_render_elements(&mut frame, 1.0, &elements, &[damage]);
        let _ = frame.finish();

        // Send frame callbacks
        let time = backend.compositor.start_time.elapsed().as_millis() as u32;
        send_frames_surface_tree(toplevel.wl_surface(), time);

        // Swap buffers for this window
        // Need to re-borrow winit after the renderer borrow is released
        if let Some(wm) = backend.window_manager.as_mut() {
            if let Some(window) = wm.windows.get_mut(&window_id) {
                if let Some(egl_surface) = &window.egl_surface {
                    if let Some(winit) = backend.graphic_renderer.as_ref() {
                        let _ = winit.submit_surface(egl_surface);
                    }
                }
            }
        }
    }
}

/// Render the main NativeActivity window (background/status).
fn render_main_window(backend: &mut WaylandBackend) {
    if let Some(winit) = backend.graphic_renderer.as_mut() {
        let size = winit.window_size();
        let damage = Rectangle::from_size(size);

        let (renderer, mut framebuffer) = match winit.bind() {
            Ok(r) => r,
            Err(e) => {
                log::error!("Failed to bind main window: {:?}", e);
                return;
            }
        };

        // Dark background for main window
        let mut frame = renderer
            .render(&mut framebuffer, size, Transform::Flipped180)
            .unwrap();
        frame
            .clear(Color32F::new(0.05, 0.05, 0.1, 1.0), &[damage])
            .unwrap();
        let _ = frame.finish();

        winit.submit(Some(&[damage])).unwrap();
    }
}

/// Handle touch events from a WaylandWindowActivity.
fn handle_activity_touch(backend: &mut WaylandBackend, window_id: u32, action: i32, x: f32, y: f32) {
    let toplevel = match get_surface_for_window(backend, window_id) {
        Some(t) => t,
        None => return,
    };

    let compositor = &mut backend.compositor;
    let serial = SERIAL_COUNTER.next_serial();
    let time = compositor.start_time.elapsed().as_millis() as u32;

    // Android MotionEvent actions
    const ACTION_DOWN: i32 = 0;
    const ACTION_UP: i32 = 1;
    const ACTION_MOVE: i32 = 2;

    match action {
        ACTION_DOWN => {
            // Set keyboard focus to this surface
            compositor.keyboard.set_focus(
                &mut compositor.state,
                Some(toplevel.wl_surface().clone()),
                serial,
            );
            // Send pointer motion + button down (emulate pointer from touch)
            let pointer = compositor.pointer.clone();
            pointer.motion(
                &mut compositor.state,
                Some((toplevel.wl_surface().clone(), (0f64, 0f64).into())),
                &pointer::MotionEvent {
                    location: (x as f64, y as f64).into(),
                    serial,
                    time,
                },
            );
            pointer.button(
                &mut compositor.state,
                &pointer::ButtonEvent {
                    button: 0x110, // BTN_LEFT
                    state: smithay::backend::input::ButtonState::Pressed.try_into().unwrap(),
                    serial,
                    time,
                },
            );
            pointer.frame(&mut compositor.state);
        }
        ACTION_UP => {
            let pointer = compositor.pointer.clone();
            pointer.button(
                &mut compositor.state,
                &pointer::ButtonEvent {
                    button: 0x110,
                    state: smithay::backend::input::ButtonState::Released.try_into().unwrap(),
                    serial,
                    time,
                },
            );
            pointer.frame(&mut compositor.state);
        }
        ACTION_MOVE => {
            let pointer = compositor.pointer.clone();
            pointer.motion(
                &mut compositor.state,
                Some((toplevel.wl_surface().clone(), (0f64, 0f64).into())),
                &pointer::MotionEvent {
                    location: (x as f64, y as f64).into(),
                    serial,
                    time,
                },
            );
            pointer.frame(&mut compositor.state);
        }
        _ => {}
    }
}

/// Handle key events from a WaylandWindowActivity.
fn handle_activity_key(backend: &mut WaylandBackend, window_id: u32, key_code: i32, action: i32) {
    let toplevel = match get_surface_for_window(backend, window_id) {
        Some(t) => t,
        None => return,
    };

    let compositor = &mut backend.compositor;
    let serial = SERIAL_COUNTER.next_serial();
    let time = compositor.start_time.elapsed().as_millis() as u32;

    // Set keyboard focus
    compositor.keyboard.set_focus(
        &mut compositor.state,
        Some(toplevel.wl_surface().clone()),
        serial,
    );

    // Android KeyEvent: ACTION_DOWN=0, ACTION_UP=1
    // Convert Android keycode to Linux keycode (approximate, offset by 7 for common keys)
    let linux_keycode = android_keycode_to_linux(key_code);
    let key_state = if action == 0 {
        smithay::backend::input::KeyState::Pressed
    } else {
        smithay::backend::input::KeyState::Released
    };

    compositor.keyboard.input::<(), _>(
        &mut compositor.state,
        linux_keycode,
        key_state,
        serial,
        time,
        |_, _, _| FilterResult::Forward,
    );
}

/// Rough Android keycode to Linux keycode conversion.
/// Android keycodes are offset from Linux keycodes by ~7 for alphanumeric keys.
fn android_keycode_to_linux(android_keycode: i32) -> u32 {
    // Android KEYCODE values → Linux KEY_ values
    // This covers the common cases; a full mapping table would be needed for production.
    let code = match android_keycode {
        // KEYCODE_0..KEYCODE_9 (7..16) → KEY_0..KEY_9 (11..20) — offset -4 doesn't work uniformly
        // Use the standard offset: Android keycodes are Linux keycodes + 7 for letter keys
        // But it's not a simple offset for all keys. For now, subtract 7.
        _ => (android_keycode - 7).max(0) as u32,
    };
    code
}

/// Handle input from the main NativeActivity (winit) — routes to first toplevel as fallback.
fn handle_winit_input(event: InputEvent<impl 'static>, backend: &mut WaylandBackend) {
    match event {
        InputEvent::Keyboard { event } => {
            let compositor = &mut backend.compositor;
            let serial = SERIAL_COUNTER.next_serial();
            let time = compositor.start_time.elapsed().as_millis() as u32;
            compositor.keyboard.input::<(), _>(
                &mut compositor.state,
                event.key_code(),
                event.state(),
                serial,
                time,
                |_, _, _| FilterResult::Forward,
            );
        }
        InputEvent::TouchDown { event } => {
            let compositor = &mut backend.compositor;
            if let Some(surface) = get_first_surface(&compositor.state) {
                compositor.keyboard.set_focus(
                    &mut compositor.state,
                    Some(surface.wl_surface().clone()),
                    0.into(),
                );
                let serial = SERIAL_COUNTER.next_serial();
                let time = compositor.start_time.elapsed().as_millis() as u32;
                compositor.touch.down(
                    &mut compositor.state,
                    Some((surface.wl_surface().clone(), (0f64, 0f64).into())),
                    &touch::DownEvent {
                        slot: event.slot(),
                        location: (event.x(), event.y()).into(),
                        serial,
                        time,
                    },
                );
            }
        }
        InputEvent::TouchUp { event } => {
            let compositor = &mut backend.compositor;
            if get_first_surface(&compositor.state).is_some() {
                let serial = SERIAL_COUNTER.next_serial();
                let time = compositor.start_time.elapsed().as_millis() as u32;
                compositor.touch.up(
                    &mut compositor.state,
                    &touch::UpEvent {
                        slot: event.slot(),
                        serial,
                        time,
                    },
                );
            }
        }
        InputEvent::TouchMotion { event } => {
            let compositor = &mut backend.compositor;
            if let Some(surface) = get_first_surface(&compositor.state) {
                let time = compositor.start_time.elapsed().as_millis() as u32;
                compositor.touch.motion(
                    &mut compositor.state,
                    Some((surface.wl_surface().clone(), (0f64, 0f64).into())),
                    &touch::MotionEvent {
                        slot: event.slot(),
                        location: (event.x(), event.y()).into(),
                        time,
                    },
                );
            }
        }
        InputEvent::PointerMotionAbsolute { event, .. } => {
            let compositor = &mut backend.compositor;
            let pointer = compositor.pointer.clone();
            let serial = SERIAL_COUNTER.next_serial();
            if let Some(surface) = get_first_surface(&compositor.state) {
                pointer.motion(
                    &mut compositor.state,
                    Some((surface.wl_surface().clone(), (0f64, 0f64).into())),
                    &pointer::MotionEvent {
                        location: (event.x(), event.y()).into(),
                        serial,
                        time: event.time_msec(),
                    },
                );
            }
            pointer.frame(&mut compositor.state);
        }
        InputEvent::PointerButton { event, .. } => {
            let serial = SERIAL_COUNTER.next_serial();
            let button = event.button_code();
            let state = ButtonState::from(event.state());
            let compositor = &mut backend.compositor;
            let pointer = compositor.pointer.clone();
            if let Some(surface) = get_first_surface(&compositor.state) {
                compositor.keyboard.set_focus(
                    &mut compositor.state,
                    Some(surface.wl_surface().clone()),
                    0.into(),
                );
            }
            pointer.button(
                &mut compositor.state,
                &pointer::ButtonEvent {
                    button,
                    state: state.try_into().unwrap(),
                    serial,
                    time: event.time_msec(),
                },
            );
            pointer.frame(&mut compositor.state);
        }
        InputEvent::PointerAxis { event } => {
            let horizontal_amount = event
                .amount(Axis::Horizontal)
                .unwrap_or_else(|| event.amount_v120(Axis::Horizontal).unwrap_or(0.0) / 120.);
            let vertical_amount = event
                .amount(Axis::Vertical)
                .unwrap_or_else(|| event.amount_v120(Axis::Vertical).unwrap_or(0.0) / 120.);
            let horizontal_amount_discrete = event.amount_v120(Axis::Horizontal);
            let vertical_amount_discrete = event.amount_v120(Axis::Vertical);

            let mut frame = pointer::AxisFrame::new(event.time_msec()).source(event.source());
            if horizontal_amount != 0.0 {
                frame = frame.relative_direction(
                    Axis::Horizontal,
                    event.relative_direction(Axis::Horizontal),
                );
                frame = frame.value(Axis::Horizontal, horizontal_amount);
                if let Some(discrete) = horizontal_amount_discrete {
                    frame = frame.v120(Axis::Horizontal, discrete as i32);
                }
            }
            if vertical_amount != 0.0 {
                frame = frame.relative_direction(
                    Axis::Vertical,
                    event.relative_direction(Axis::Vertical),
                );
                frame = frame.value(Axis::Vertical, vertical_amount);
                if let Some(discrete) = vertical_amount_discrete {
                    frame = frame.v120(Axis::Vertical, discrete as i32);
                }
            }
            if event.amount(Axis::Horizontal) == Some(0.0) {
                frame = frame.stop(Axis::Horizontal);
            }
            if event.amount(Axis::Vertical) == Some(0.0) {
                frame = frame.stop(Axis::Vertical);
            }
            let compositor = &mut backend.compositor;
            let pointer = compositor.pointer.clone();
            pointer.axis(&mut compositor.state, frame);
            pointer.frame(&mut compositor.state);
        }
        _ => {}
    }
}
