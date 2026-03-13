use crate::android::backend::WaylandBackend;
use crate::android::compositor::{send_frames_surface_tree, ClientState};
use crate::android::window_manager::WindowEvent;
use smithay::backend::input::ButtonState;
use smithay::backend::renderer::element::surface::{
    render_elements_from_surface_tree, WaylandSurfaceRenderElement,
};
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::utils::draw_render_elements;
use smithay::backend::renderer::{Color32F, Frame, Renderer};
use smithay::input::keyboard::FilterResult;
use smithay::input::pointer;
use smithay::utils::{Rectangle, Transform, SERIAL_COUNTER};
use smithay::wayland::compositor as wl_compositor;
use smithay::wayland::shell::xdg::{SurfaceCachedState, ToplevelSurface, XdgToplevelSurfaceData};
use std::sync::Arc;
use std::time::Instant;

/// One iteration of the compositor loop: dispatch protocol, render, update status.
pub fn compositor_tick(backend: &mut WaylandBackend) {
    dispatch_wayland(backend);
    render_activity_windows(backend);
    update_status_overlay(backend);
}

/// Process Wayland protocol: accept new clients, dispatch messages, flush responses.
/// Called from both the redraw path and proxy_wake_up so the compositor keeps
/// working even when the NativeActivity window is destroyed.
pub fn dispatch_wayland(backend: &mut WaylandBackend) {
    // Process pending toplevels: create Activity windows.
    let pending: Vec<ToplevelSurface> =
        backend.compositor.state.pending_toplevels.drain(..).collect();
    if !pending.is_empty() {
        if let Some(wm) = backend.window_manager.as_mut() {
            for toplevel in pending {
                wm.new_toplevel(toplevel);
            }
        }
    }

    // Process window events from JNI.
    process_window_events(backend);

    // Accept Wayland clients, dispatch protocol.
    match backend.compositor.listener.accept() {
        Ok(Some(stream)) => {
            match backend
                .compositor
                .display
                .handle()
                .insert_client(stream, Arc::new(ClientState::default()))
            {
                Ok(client) => backend.compositor.clients.push(client),
                Err(e) => log::error!("Failed to insert client: {:?}", e),
            }
        }
        Ok(None) => {}
        Err(e) => log::error!("Failed to accept listener: {:?}", e),
    }

    if let Err(e) = backend
        .compositor
        .display
        .dispatch_clients(&mut backend.compositor.state)
    {
        log::error!("Failed to dispatch clients: {:?}", e);
    }
    // Process soft keyboard show/hide from text_input_v3.
    if let Some(visible) = backend.compositor.state.soft_keyboard_request.take() {
        let window_id = backend
            .compositor
            .keyboard
            .as_ref()
            .and_then(|kb| kb.current_focus())
            .and_then(|focused| {
                backend.window_manager.as_ref().and_then(|wm| {
                    wm.windows
                        .iter()
                        .find_map(|(id, w)| (w.toplevel.wl_surface() == &focused).then_some(*id))
                })
            });
        if let Some(window_id) = window_id {
            if let Err(e) = set_soft_keyboard_visible(window_id, visible) {
                log::error!("Failed to set soft keyboard visibility: {e}");
            }
        }
    }

    if let Err(e) = backend
        .compositor
        .display
        .flush_clients()
    {
        log::error!("Failed to flush clients: {:?}", e);
    }
}

/// Process events from WaylandWindowActivity JNI callbacks.
fn process_window_events(backend: &mut WaylandBackend) {
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
                // Store the native window pointer first
                if let Some(wm) = backend.window_manager.as_mut()
                    && let Some(window) = wm.windows.get_mut(&window_id) {
                        window.native_window = Some(native_window);
                    }
                // Now create EGL surface (needs both winit and wm)
                let handle = backend
                    .window_manager
                    .as_ref()
                    .and_then(|wm| wm.get_native_handle(window_id));

                if let Some(handle) = handle {
                    let surface = backend
                        .renderer
                        .as_ref()
                        .and_then(|r| r.create_surface_for_native_window(handle).ok());

                    if let Some(surface) = surface {
                        log::info!("Created EGL surface for window_id={}", window_id);
                        if let Some(wm) = backend.window_manager.as_mut()
                            && let Some(window) = wm.windows.get_mut(&window_id) {
                                window.egl_surface = Some(surface);
                            }
                    } else {
                        log::error!("Failed to create EGL surface for window_id={}", window_id);
                    }
                }
            }
            WindowEvent::SurfaceChanged {
                window_id,
                width,
                height,
            } => {
                let scale = backend.scale_factor;
                if let Some(wm) = backend.window_manager.as_mut()
                    && let Some(window) = wm.windows.get_mut(&window_id) {
                        window.size = (width, height).into();
                        window.needs_redraw = true;
                        // Configure toplevel with logical size so apps render
                        // at the right scale for the display density.
                        let logical_w = (width as f64 / scale).round() as i32;
                        let logical_h = (height as f64 / scale).round() as i32;
                        window.toplevel.with_pending_state(|state| {
                            state.size = Some((logical_w, logical_h).into());
                        });
                        window.toplevel.send_configure();
                        // Set preferred fractional scale on the toplevel surface.
                        wl_compositor::with_states(window.toplevel.wl_surface(), |states| {
                            smithay::wayland::fractional_scale::with_fractional_scale(
                                states,
                                |fs| fs.set_preferred_scale(scale),
                            );
                        });
                        log::info!("Window {} resized to {}x{} (logical {}x{}, scale {})",
                            window_id, width, height, logical_w, logical_h, scale);
                    }
            }
            WindowEvent::SurfaceDestroyed { window_id } => {
                if let Some(wm) = backend.window_manager.as_mut()
                    && let Some(window) = wm.windows.get_mut(&window_id) {
                        window.egl_surface = None;
                        window.native_window = None;
                        log::info!("Surface destroyed for window_id={}", window_id);
                    }
            }
            WindowEvent::WindowClosed { window_id, is_finishing } => {
                if is_finishing {
                    // User actually closed the window — tell the Wayland client
                    if let Some(wm) = backend.window_manager.as_ref()
                        && let Some(window) = wm.windows.get(&window_id) {
                            window.toplevel.send_close();
                        }
                    if let Some(wm) = backend.window_manager.as_mut() {
                        wm.remove_window(window_id);
                    }
                } else {
                    log::info!("Window {} destroyed by Android (config change), keeping toplevel alive", window_id);
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
            WindowEvent::RightClick {
                window_id,
                x,
                y,
            } => {
                handle_activity_right_click(backend, window_id, x, y);
            }
        }
    }
}

/// Render each Activity window's toplevel to its EGL surface.
fn render_activity_windows(backend: &mut WaylandBackend) {
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

    let time = backend.compositor.start_time.elapsed().as_millis() as u32;
    let scale = backend.scale_factor;

    for window_id in window_ids {
        // Get the toplevel and size from window manager
        let (toplevel, size, geo_offset) = {
            let wm = match backend.window_manager.as_ref() {
                Some(wm) => wm,
                None => continue,
            };
            let window = match wm.windows.get(&window_id) {
                Some(w) => w,
                None => continue,
            };
            // Get the geometry (content area excluding CSD shadows).
            let geo_loc = wl_compositor::with_states(window.toplevel.wl_surface(), |states| {
                states.cached_state.get::<SurfaceCachedState>()
                    .current()
                    .geometry
                    .map(|g| g.loc)
                    .unwrap_or_default()
            });
            (window.toplevel.clone(), window.size, geo_loc)
        };

        let damage = Rectangle::from_size(size);

        // Render in a scoped block so borrows are released before submit
        {
            let Some(wm) = backend.window_manager.as_mut() else {
                continue;
            };
            let Some(window) = wm.windows.get_mut(&window_id) else {
                continue;
            };
            let Some(egl_surface) = window.egl_surface.as_mut() else {
                continue;
            };
            let Some(cr) = backend.renderer.as_mut() else {
                continue;
            };

            let Ok((renderer, mut framebuffer)) = cr.bind_surface(egl_surface) else {
                log::error!("Failed to bind surface for window_id={}", window_id);
                continue;
            };

            // Offset by negative geometry origin, scaled to physical pixels.
            let render_offset = (
                ((-geo_offset.x) as f64 * scale).round() as i32,
                ((-geo_offset.y) as f64 * scale).round() as i32,
            );
            let elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
                render_elements_from_surface_tree(
                    renderer,
                    toplevel.wl_surface(),
                    render_offset,
                    scale,
                    1.0,
                    Kind::Unspecified,
                );

            let Ok(mut frame) = renderer.render(&mut framebuffer, size, Transform::Flipped180)
            else {
                log::error!("Failed to begin render for window_id={}", window_id);
                continue;
            };

            let _ = frame.clear(Color32F::new(0.0, 0.0, 0.0, 1.0), &[damage]);
            let _ = draw_render_elements(&mut frame, scale, &elements, &[damage]);
            let _ = frame.finish();

            send_frames_surface_tree(toplevel.wl_surface(), time);
        }
        // Borrows released — now swap buffers
        {
            let Some(wm) = backend.window_manager.as_ref() else {
                continue;
            };
            let Some(window) = wm.windows.get(&window_id) else {
                continue;
            };
            let Some(egl_surface) = window.egl_surface.as_ref() else {
                continue;
            };
            let Some(cr) = backend.renderer.as_ref() else {
                continue;
            };
            let _ = cr.submit_surface(egl_surface);
        }
        // Count rendered frame for FPS tracking.
        if let Some(wm) = backend.window_manager.as_mut()
            && let Some(window) = wm.windows.get_mut(&window_id)
        {
            window.frame_count += 1;
        }
    }
}

/// Update the status overlay on the main NativeActivity with client info and FPS.
fn update_status_overlay(backend: &mut WaylandBackend) {
    static LAST_UPDATE: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);

    let Ok(mut last) = LAST_UPDATE.lock() else { return };
    let now = Instant::now();
    let elapsed_secs = last.map(|t| now.duration_since(t).as_secs_f64()).unwrap_or(0.0);
    if elapsed_secs > 0.0 && elapsed_secs < 1.0 {
        return;
    }
    *last = Some(now);
    drop(last);

    let toplevels = backend.compositor.state.xdg_shell_state.toplevel_surfaces();
    let num_clients = backend.compositor.clients.len();
    let num_toplevels = toplevels.len();

    let mut info = format!("Clients: {}  Toplevels: {}  Scale: {:.2}\n", num_clients, num_toplevels, backend.scale_factor);

    if let Some(wm) = backend.window_manager.as_mut() {
        for (id, window) in &mut wm.windows {
            let title = wl_compositor::with_states(window.toplevel.wl_surface(), |states| {
                states
                    .data_map
                    .get::<XdgToplevelSurfaceData>()
                    .and_then(|data| data.lock().ok())
                    .and_then(|attrs| attrs.title.clone())
                    .unwrap_or_default()
            });
            let has_surface = window.egl_surface.is_some();
            let fps = if elapsed_secs > 0.0 {
                window.frame_count as f64 / elapsed_secs
            } else {
                0.0
            };
            window.frame_count = 0;
            info.push_str(&format!(
                "  [{}] {}  {}x{}  {}  {:.1} fps\n",
                id,
                if title.is_empty() { "(untitled)" } else { &title },
                window.size.w,
                window.size.h,
                if has_surface { "visible" } else { "hidden" },
                fps,
            ));
        }
    }

    log::debug!("Status: {}", info.trim());
    if let Err(e) = send_status_jni(&info) {
        log::error!("Status overlay JNI call failed: {e}");
    }
}

fn send_status_jni(text: &str) -> Result<(), jni::errors::Error> {
    crate::android::utils::jni_context::with_jni(|env, activity| {
        let class_loader = env
            .call_method(activity, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])?
            .l()?;
        let class_name = env.new_string("io.github.phiresky.wayland_android.MainActivity")?;
        let main_class = env
            .call_method(
                &class_loader,
                "loadClass",
                "(Ljava/lang/String;)Ljava/lang/Class;",
                &[jni::objects::JValue::Object(&class_name)],
            )?
            .l()?;

        let jtext = env.new_string(text)?;
        env.call_static_method(
            unsafe { jni::objects::JClass::from_raw(main_class.as_raw()) },
            "updateStatus",
            "(Ljava/lang/String;)V",
            &[jni::objects::JValue::Object(&jtext)],
        )?;

        Ok(())
    })
}

/// Show or hide the Android soft keyboard on a specific WaylandWindowActivity.
fn set_soft_keyboard_visible(
    window_id: u32,
    visible: bool,
) -> Result<(), jni::errors::Error> {
    crate::android::utils::jni_context::with_jni(|env, activity| {
        let class_loader = env
            .call_method(
                activity,
                "getClassLoader",
                "()Ljava/lang/ClassLoader;",
                &[],
            )?
            .l()?;
        let class_name =
            env.new_string("io.github.phiresky.wayland_android.WaylandWindowActivity")?;
        let window_class = env
            .call_method(
                &class_loader,
                "loadClass",
                "(Ljava/lang/String;)Ljava/lang/Class;",
                &[jni::objects::JValue::Object(&class_name)],
            )?
            .l()?;

        env.call_static_method(
            unsafe { jni::objects::JClass::from_raw(window_class.as_raw()) },
            "setSoftKeyboardVisible",
            "(IZ)V",
            &[
                jni::objects::JValue::Int(window_id as i32),
                jni::objects::JValue::Bool(u8::from(visible)),
            ],
        )?;

        Ok(())
    })
}

/// Handle touch events from a WaylandWindowActivity.
fn handle_activity_touch(
    backend: &mut WaylandBackend,
    window_id: u32,
    action: i32,
    x: f32,
    y: f32,
) {
    let toplevel = {
        let wm = match backend.window_manager.as_ref() {
            Some(wm) => wm,
            None => return,
        };
        match wm.windows.get(&window_id) {
            Some(w) => w.toplevel.clone(),
            None => return,
        }
    };

    // Convert from physical (Android) to logical (Wayland surface) coordinates.
    // Add geometry offset because we render at -geo_offset to crop CSD shadows,
    // so touch position 0,0 in the Activity corresponds to geo_offset in the surface.
    let scale = backend.scale_factor;
    let geo_offset = wl_compositor::with_states(toplevel.wl_surface(), |states| {
        states.cached_state.get::<SurfaceCachedState>()
            .current()
            .geometry
            .map(|g| g.loc)
            .unwrap_or_default()
    });
    let x = x as f64 / scale + geo_offset.x as f64;
    let y = y as f64 / scale + geo_offset.y as f64;

    let compositor = &mut backend.compositor;
    let serial = SERIAL_COUNTER.next_serial();
    let time = compositor.start_time.elapsed().as_millis() as u32;

    // Android MotionEvent actions
    const ACTION_DOWN: i32 = 0;
    const ACTION_UP: i32 = 1;
    const ACTION_MOVE: i32 = 2;

    match action {
        ACTION_DOWN => {
            if let Some(kb) = &compositor.keyboard {
                kb.set_focus(
                    &mut compositor.state,
                    Some(toplevel.wl_surface().clone()),
                    serial,
                );
            }
            let pointer = compositor.pointer.clone();
            pointer.motion(
                &mut compositor.state,
                Some((toplevel.wl_surface().clone(), (0f64, 0f64).into())),
                &pointer::MotionEvent {
                    location: (x, y).into(),
                    serial,
                    time,
                },
            );
            pointer.button(
                &mut compositor.state,
                &pointer::ButtonEvent {
                    button: 0x110, // BTN_LEFT
                    state: ButtonState::Pressed,
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
                    state: ButtonState::Released,
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
                    location: (x, y).into(),
                    serial,
                    time,
                },
            );
            pointer.frame(&mut compositor.state);
        }
        _ => {}
    }
}

/// Handle a right-click from the long-press context menu.
/// Sends pointer motion + BTN_RIGHT press + release to the Wayland client.
fn handle_activity_right_click(
    backend: &mut WaylandBackend,
    window_id: u32,
    x: f32,
    y: f32,
) {
    let toplevel = {
        let wm = match backend.window_manager.as_ref() {
            Some(wm) => wm,
            None => return,
        };
        match wm.windows.get(&window_id) {
            Some(w) => w.toplevel.clone(),
            None => return,
        }
    };

    let scale = backend.scale_factor;
    let geo_offset = wl_compositor::with_states(toplevel.wl_surface(), |states| {
        states.cached_state.get::<SurfaceCachedState>()
            .current()
            .geometry
            .map(|g| g.loc)
            .unwrap_or_default()
    });
    let x = x as f64 / scale + geo_offset.x as f64;
    let y = y as f64 / scale + geo_offset.y as f64;

    let compositor = &mut backend.compositor;
    let serial = SERIAL_COUNTER.next_serial();
    let time = compositor.start_time.elapsed().as_millis() as u32;

    let pointer = compositor.pointer.clone();

    // Move pointer to the right-click position.
    pointer.motion(
        &mut compositor.state,
        Some((toplevel.wl_surface().clone(), (0f64, 0f64).into())),
        &pointer::MotionEvent {
            location: (x, y).into(),
            serial,
            time,
        },
    );
    // Press BTN_RIGHT.
    pointer.button(
        &mut compositor.state,
        &pointer::ButtonEvent {
            button: 0x111, // BTN_RIGHT
            state: ButtonState::Pressed,
            serial,
            time,
        },
    );
    // Release BTN_RIGHT.
    let serial = SERIAL_COUNTER.next_serial();
    pointer.button(
        &mut compositor.state,
        &pointer::ButtonEvent {
            button: 0x111,
            state: ButtonState::Released,
            serial,
            time,
        },
    );
    pointer.frame(&mut compositor.state);
}

/// Handle key events from a WaylandWindowActivity.
fn handle_activity_key(
    backend: &mut WaylandBackend,
    window_id: u32,
    key_code: i32,
    action: i32,
) {
    let toplevel = {
        let wm = match backend.window_manager.as_ref() {
            Some(wm) => wm,
            None => return,
        };
        match wm.windows.get(&window_id) {
            Some(w) => w.toplevel.clone(),
            None => return,
        }
    };

    let compositor = &mut backend.compositor;
    let serial = SERIAL_COUNTER.next_serial();
    let time = compositor.start_time.elapsed().as_millis() as u32;

    // Set keyboard focus only if this surface doesn't already have it.
    // Calling set_focus on every key event floods the client with keymap data and causes ANR.
    if let Some(kb) = &compositor.keyboard {
        let wl_surface = toplevel.wl_surface().clone();
        let needs_focus = kb.current_focus().as_ref() != Some(&wl_surface);
        if needs_focus {
            kb.set_focus(&mut compositor.state, Some(wl_surface), serial);
        }
    }

    let Some(linux_keycode) = super::keymap::android_keycode_to_smithay(key_code) else {
        log::debug!("Unmapped Android keycode: {}", key_code);
        return;
    };
    let key_state = if action == 0 {
        smithay::backend::input::KeyState::Pressed
    } else {
        smithay::backend::input::KeyState::Released
    };

    if let Some(kb) = &compositor.keyboard {
        kb.input::<(), _>(
            &mut compositor.state,
            linux_keycode.into(),
            key_state,
            serial,
            time,
            |_, _, _| FilterResult::Forward,
        );
    }
}


