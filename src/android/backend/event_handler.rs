use crate::android::backend::WaylandBackend;
use crate::android::compositor::{send_frames_surface_tree, ClientState};
use crate::android::window_manager::{SurfaceKind, WindowEvent};
use smithay::backend::input::ButtonState;
use smithay::backend::renderer::element::surface::{
    render_elements_from_surface_tree, WaylandSurfaceRenderElement,
};
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::utils::draw_render_elements;
use smithay::backend::renderer::{Color32F, Frame, Renderer};
use smithay::desktop::{utils::under_from_surface_tree, PopupManager, WindowSurfaceType};
use smithay::input::keyboard::FilterResult;
use smithay::input::pointer;
use smithay::utils::{Rectangle, Transform, SERIAL_COUNTER};
use smithay::wayland::compositor as wl_compositor;
use smithay::wayland::shell::wlr_layer::LayerSurface;
use smithay::wayland::shell::xdg::{SurfaceCachedState, ToplevelSurface, XdgToplevelSurfaceData};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::Point;
use std::sync::Arc;
use std::time::Instant;

const WINDOW_ACTIVITY_CLASS: &str = "io.github.phiresky.wayland_android.WaylandWindowActivity";

// Linux input event button codes.
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;

// Android MotionEvent action constants.
const ACTION_DOWN: i32 = 0;
const ACTION_UP: i32 = 1;
const ACTION_MOVE: i32 = 2;

/// Height of the Samsung DeX window title bar in physical pixels.
/// setLaunchBounds specifies outer window bounds (including chrome),
/// so we add this to the content height to get the correct content area.
const DEX_TITLE_BAR_HEIGHT: i32 = 70;

/// One iteration of the compositor loop: dispatch protocol, render, update status.
pub fn compositor_tick(backend: &mut WaylandBackend) {
    dispatch_wayland(backend);
    render_activity_windows(backend);
    update_status_overlay(backend);
}

/// Process Wayland protocol: accept new clients, dispatch messages, flush responses.
/// Called from both the redraw path and proxy_wake_up so the compositor keeps
/// working even when the Activity window is destroyed.
pub fn dispatch_wayland(backend: &mut WaylandBackend) {
    // Process pending surfaces: create Activity windows for new toplevels and layers.
    let pending: Vec<SurfaceKind> = backend.compositor.state.pending_toplevels
        .drain(..).map(SurfaceKind::Toplevel)
        .chain(backend.compositor.state.pending_layer_surfaces.drain(..).map(SurfaceKind::Layer))
        .collect();
    if !pending.is_empty() {
        if let Some(wm) = backend.window_manager.as_mut() {
            for surface in pending {
                wm.new_window(surface);
            }
        }
    }

    // Launch Activities for windows whose clients have committed (geometry available),
    // or after a timeout for slow clients. Uses setLaunchBounds for correct DeX sizing.
    launch_pending_activities(backend);

    // Process destroyed surfaces (toplevels and layers): finish their Android Activities.
    let destroyed_toplevels: Vec<ToplevelSurface> =
        backend.compositor.state.destroyed_toplevels.drain(..).collect();
    let destroyed_layers: Vec<LayerSurface> =
        backend.compositor.state.destroyed_layer_surfaces.drain(..).collect();
    if !destroyed_toplevels.is_empty() || !destroyed_layers.is_empty() {
        if let Some(wm) = backend.window_manager.as_mut() {
            // Build predicates that match the destroyed surface to its window.
            let lookups: Vec<(&str, Option<u32>)> = destroyed_toplevels.iter()
                .map(|t| ("Toplevel", wm.find_window_id(|sk| matches!(sk, SurfaceKind::Toplevel(wt) if *wt == *t))))
                .chain(destroyed_layers.iter()
                    .map(|l| ("Layer surface", wm.find_window_id(|sk| matches!(sk, SurfaceKind::Layer(wl) if *wl == *l)))))
                .collect();
            for (kind, window_id) in lookups {
                if let Some(window_id) = window_id {
                    log::info!("{kind} destroyed, finishing Activity window_id={window_id}");
                    if let Err(e) = finish_activity(window_id) {
                        log::error!("Failed to finish Activity for window_id={window_id}: {e}");
                    }
                    wm.remove_window(window_id);
                }
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
                        .find_map(|(id, w)| (w.surface_kind.wl_surface() == &focused).then_some(*id))
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
                // Now create EGL surface (needs both renderer and wm)
                let handle = backend
                    .window_manager
                    .as_ref()
                    .and_then(|wm| wm.get_native_handle(window_id));

                if let Some(handle) = handle {
                    // Test Vulkan presentation (clear to cornflower blue)
                    if let Some(ref vk) = backend.vk_renderer {
                        let raw_window = handle.a_native_window.as_ptr();
                        match vk.create_window_surface(raw_window) {
                            Ok(vk_surface) => {
                                log::info!("Vulkan surface created for window_id={}", window_id);
                                match vk.present_clear_color(&vk_surface, 0.39, 0.58, 0.93) {
                                    Ok(()) => log::info!("Vulkan clear color presented!"),
                                    Err(e) => log::error!("Vulkan present failed: {e}"),
                                }
                                // TODO: store vk_surface for ongoing rendering
                            }
                            Err(e) => log::error!("Vulkan surface creation failed: {e}"),
                        }
                    }

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
                        let logical_w = (width as f64 / scale).round() as i32;
                        let logical_h_from_content = (height as f64 / scale).round() as i32;
                        // If the user has resized the window (width changed or height grew
                        // beyond the initial preferred size), clear preferred_size so the
                        // client fills the new window without artificial caps or centering.
                        if let Some(p) = window.preferred_size {
                            if logical_w != p.w || logical_h_from_content > p.h {
                                window.preferred_size = None;
                            }
                        }
                        // If we know the client's preferred size (set at launch), cap the
                        // configure to that height. DeX enforces a minimum window height
                        // larger than small dialogs need, so we avoid over-configuring.
                        let logical_h = window.preferred_size
                            .map(|p| p.h.min(logical_h_from_content))
                            .unwrap_or(logical_h_from_content);
                        match &window.surface_kind {
                            SurfaceKind::Toplevel(toplevel) => {
                                // Configure toplevel with logical size so apps render
                                // at the right scale for the display density.
                                log::info!("SurfaceChanged window_id={window_id}: physical={width}x{height} -> configure logical={logical_w}x{logical_h} (preferred={:?}, scale={scale})",
                                    window.preferred_size);
                                toplevel.with_pending_state(|state| {
                                    state.size = Some((logical_w, logical_h).into());
                                });
                                toplevel.send_configure();
                            }
                            SurfaceKind::Layer(layer) => {
                                // Configure layer surface with the Activity size so
                                // anchored surfaces know the available area.
                                layer.with_pending_state(|state| {
                                    state.size = Some((logical_w, logical_h).into());
                                });
                                layer.send_pending_configure();
                            }
                        }
                        // Set preferred fractional scale on the surface.
                        wl_compositor::with_states(window.surface_kind.wl_surface(), |states| {
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
                            window.surface_kind.send_close();
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
                ..
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
        // Get the wl_surface, size, and preferred_size from window manager
        let (wl_surface, size, geo_offset, preferred_size) = {
            let wm = match backend.window_manager.as_ref() {
                Some(wm) => wm,
                None => continue,
            };
            let window = match wm.windows.get(&window_id) {
                Some(w) => w,
                None => continue,
            };
            let surface = window.surface_kind.wl_surface().clone();
            // Get the geometry origin (content area excluding CSD shadows).
            // Only toplevels have XDG geometry; layer surfaces don't.
            let geo_offset = match &window.surface_kind {
                SurfaceKind::Toplevel(_) => {
                    wl_compositor::with_states(&surface, |states| {
                        states.cached_state.get::<SurfaceCachedState>()
                            .current()
                            .geometry
                            .map(|g| g.loc)
                            .unwrap_or_default()
                    })
                }
                SurfaceKind::Layer(_) => Default::default(),
            };
            (surface, window.size, geo_offset, window.preferred_size)
        };

        let damage = Rectangle::from_size(size);

        // If DeX gave us a taller window than the client wants, center content vertically.
        let center_y_physical = preferred_size.map(|p| {
            let preferred_h = (p.h as f64 * scale).round() as i32;
            ((size.h - preferred_h) / 2).max(0)
        }).unwrap_or(0);

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
            // Add vertical centering so the content sits in the middle of the
            // EGL surface when DeX enforces a minimum height larger than the client wants.
            let render_offset = (
                ((-geo_offset.x) as f64 * scale).round() as i32,
                ((-geo_offset.y) as f64 * scale).round() as i32 + center_y_physical,
            );
            // Collect popup elements first (rendered on top of the main surface).
            let mut elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>> = Vec::new();
            for (popup, popup_offset) in PopupManager::popups_for_surface(&wl_surface) {
                let offset = popup_offset - popup.geometry().loc + geo_offset;
                let popup_render_offset = (
                    ((-geo_offset.x + offset.x) as f64 * scale).round() as i32,
                    ((-geo_offset.y + offset.y) as f64 * scale).round() as i32,
                );
                elements.extend(render_elements_from_surface_tree(
                    renderer,
                    popup.wl_surface(),
                    popup_render_offset,
                    scale,
                    1.0,
                    Kind::Unspecified,
                ));
            }
            elements.extend(render_elements_from_surface_tree(
                    renderer,
                    &wl_surface,
                    render_offset,
                    scale,
                    1.0,
                    Kind::Unspecified,
                ));

            let Ok(mut frame) = renderer.render(&mut framebuffer, size, Transform::Flipped180)
            else {
                log::error!("Failed to begin render for window_id={}", window_id);
                continue;
            };

            if let Err(e) = frame.clear(Color32F::new(0.0, 0.0, 0.0, 1.0), &[damage]) {
                log::warn!("frame.clear failed for window_id={}: {e:?}", window_id);
            }
            if let Err(e) = draw_render_elements(&mut frame, scale, &elements, &[damage]) {
                log::warn!("draw_render_elements failed for window_id={}: {e:?}", window_id);
            }
            if let Err(e) = frame.finish() {
                log::warn!("frame.finish failed for window_id={}: {e:?}", window_id);
            }

            send_frames_surface_tree(&wl_surface, time);
            // Also send frame callbacks for popup surfaces (menus, dropdowns).
            for (popup, _) in PopupManager::popups_for_surface(&wl_surface) {
                send_frames_surface_tree(popup.wl_surface(), time);
            }
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

/// Update the status overlay on the MainActivity with client info and FPS.
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
            let title = wl_compositor::with_states(window.surface_kind.wl_surface(), |states| {
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
        let class = crate::android::utils::jni_context::load_class(
            env, activity, "io.github.phiresky.wayland_android.MainActivity",
        )?;
        let jtext = env.new_string(text)?;
        env.call_static_method(
            class,
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
        let class = crate::android::utils::jni_context::load_class(
            env, activity, WINDOW_ACTIVITY_CLASS,
        )?;
        env.call_static_method(
            class,
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

/// Launch Activities for windows whose clients have committed geometry,
/// or after a timeout for slow clients. Uses setLaunchBounds for correct DeX sizing.
fn launch_pending_activities(backend: &mut WaylandBackend) {
    use smithay::utils::Size;
    let scale = backend.scale_factor;
    // Collect (window_id, bounds, preferred_logical_size)
    let pending: Vec<(u32, Option<(i32, i32)>, Option<Size<i32, smithay::utils::Logical>>)> =
        backend.window_manager.as_ref()
        .map(|wm| {
            wm.windows.iter()
                .filter(|(_, w)| !w.activity_launched)
                .filter_map(|(&id, w)| {
                    // Read the client's committed geometry (if any).
                    let geo_size = match &w.surface_kind {
                        SurfaceKind::Toplevel(_) => {
                            wl_compositor::with_states(w.surface_kind.wl_surface(), |states| {
                                states.cached_state.get::<SurfaceCachedState>()
                                    .current()
                                    .geometry
                                    .map(|g| g.size)
                            })
                        }
                        SurfaceKind::Layer(_) => None,
                    };
                    let bounds = geo_size.map(|s| {
                        ((s.w as f64 * scale).round() as i32,
                         (s.h as f64 * scale).round() as i32 + DEX_TITLE_BAR_HEIGHT)
                    }).filter(|&(w, h)| w > 0 && h > 0);

                    log::info!("launch_pending: window_id={id} geo_size={geo_size:?} bounds={bounds:?} elapsed={}ms",
                        w.created_time.elapsed().as_millis());
                    if bounds.is_some() {
                        // Client has committed with geometry — launch with bounds.
                        Some((id, bounds, geo_size))
                    } else if w.created_time.elapsed() > std::time::Duration::from_millis(500) {
                        // Timeout — launch without bounds (full-size fallback).
                        Some((id, None, None))
                    } else {
                        None // Still waiting for client to commit.
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    if let Some(wm) = backend.window_manager.as_mut() {
        for (window_id, bounds, preferred) in pending {
            // Store preferred logical size so SurfaceChanged doesn't over-configure.
            // DeX enforces a minimum window height larger than small dialogs need;
            // we cap the Wayland configure to the client's preferred size instead.
            if let Some(pref) = preferred {
                if let Some(w) = wm.windows.get_mut(&window_id) {
                    w.preferred_size = Some(pref);
                }
            }
            wm.launch_activity(window_id, bounds);
        }
    }
}

/// Finish the Android Activity for a given window ID via JNI.
fn finish_activity(window_id: u32) -> Result<(), jni::errors::Error> {
    crate::android::utils::jni_context::with_jni(|env, activity| {
        let class = crate::android::utils::jni_context::load_class(
            env, activity, WINDOW_ACTIVITY_CLASS,
        )?;
        env.call_static_method(
            class,
            "finishByWindowId",
            "(I)V",
            &[jni::objects::JValue::Int(window_id as i32)],
        )?;
        Ok(())
    })
}

/// Look up the Wayland surface under a touch point, converting physical Android
/// coordinates to logical Wayland coordinates. Checks popups first, then the
/// main surface tree, so menu clicks are routed to the popup surface.
fn resolve_surface_and_coords(
    backend: &WaylandBackend,
    window_id: u32,
    x: f32,
    y: f32,
) -> Option<(WlSurface, f64, f64)> {
    let wm = backend.window_manager.as_ref()?;
    let window = wm.windows.get(&window_id)?;
    let root_surface = window.surface_kind.wl_surface().clone();
    let geo_offset: Point<i32, _> = match &window.surface_kind {
        SurfaceKind::Toplevel(_) => {
            wl_compositor::with_states(&root_surface, |states| {
                states.cached_state.get::<SurfaceCachedState>()
                    .current()
                    .geometry
                    .map(|g| g.loc)
                    .unwrap_or_default()
            })
        }
        SurfaceKind::Layer(_) => Default::default(),
    };
    // Convert from physical (Android) to logical (Wayland surface) coordinates.
    // Touch position 0,0 in the Activity corresponds to geo_offset in the surface.
    // If content is centered vertically (due to DeX minimum height), subtract that offset.
    let scale = backend.scale_factor;
    let center_y_logical = window.preferred_size.map(|p| {
        let preferred_h = (p.h as f64 * scale).round() as i32;
        ((window.size.h - preferred_h) / 2).max(0) as f64 / scale
    }).unwrap_or(0.0);
    let lx = x as f64 / scale + geo_offset.x as f64;
    let ly = y as f64 / scale - center_y_logical + geo_offset.y as f64;
    let point: Point<f64, _> = (lx, ly).into();

    // Check popups first — menus/dropdowns take priority over the main surface.
    for (popup, popup_offset) in PopupManager::popups_for_surface(&root_surface) {
        let offset = geo_offset + popup_offset - popup.geometry().loc;
        if let Some((surface, surface_offset)) = under_from_surface_tree(
            popup.wl_surface(),
            point,
            offset,
            WindowSurfaceType::ALL,
        ) {
            let sx = point.x - surface_offset.x as f64;
            let sy = point.y - surface_offset.y as f64;
            return Some((surface, sx, sy));
        }
    }

    // Fall back to the main surface tree (toplevel + subsurfaces).
    if let Some((surface, surface_offset)) = under_from_surface_tree(
        &root_surface,
        point,
        (0, 0),
        WindowSurfaceType::ALL,
    ) {
        let sx = point.x - surface_offset.x as f64;
        let sy = point.y - surface_offset.y as f64;
        return Some((surface, sx, sy));
    }

    // Nothing under the point — fall back to root surface at logical coords.
    Some((root_surface, lx, ly))
}

/// Handle touch events from a WaylandWindowActivity.
fn handle_activity_touch(
    backend: &mut WaylandBackend,
    window_id: u32,
    action: i32,
    x: f32,
    y: f32,
) {
    let Some((wl_surface, x, y)) = resolve_surface_and_coords(backend, window_id, x, y) else {
        return;
    };

    let compositor = &mut backend.compositor;
    let serial = SERIAL_COUNTER.next_serial();
    let time = compositor.start_time.elapsed().as_millis() as u32;
    let pointer = compositor.pointer.clone();

    match action {
        ACTION_DOWN => {
            if let Some(kb) = &compositor.keyboard {
                kb.set_focus(
                    &mut compositor.state,
                    Some(wl_surface.clone()),
                    serial,
                );
            }
            pointer.motion(
                &mut compositor.state,
                Some((wl_surface.clone(), (0f64, 0f64).into())),
                &pointer::MotionEvent {
                    location: (x, y).into(),
                    serial,
                    time,
                },
            );
            pointer.button(
                &mut compositor.state,
                &pointer::ButtonEvent {
                    button: BTN_LEFT,
                    state: ButtonState::Pressed,
                    serial,
                    time,
                },
            );
            pointer.frame(&mut compositor.state);
        }
        ACTION_UP => {
            pointer.button(
                &mut compositor.state,
                &pointer::ButtonEvent {
                    button: BTN_LEFT,
                    state: ButtonState::Released,
                    serial,
                    time,
                },
            );
            pointer.frame(&mut compositor.state);
        }
        ACTION_MOVE => {
            pointer.motion(
                &mut compositor.state,
                Some((wl_surface.clone(), (0f64, 0f64).into())),
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
    let Some((wl_surface, x, y)) = resolve_surface_and_coords(backend, window_id, x, y) else {
        return;
    };

    let compositor = &mut backend.compositor;
    let serial = SERIAL_COUNTER.next_serial();
    let time = compositor.start_time.elapsed().as_millis() as u32;
    let pointer = compositor.pointer.clone();

    pointer.motion(
        &mut compositor.state,
        Some((wl_surface.clone(), (0f64, 0f64).into())),
        &pointer::MotionEvent {
            location: (x, y).into(),
            serial,
            time,
        },
    );
    pointer.button(
        &mut compositor.state,
        &pointer::ButtonEvent {
            button: BTN_RIGHT,
            state: ButtonState::Pressed,
            serial,
            time,
        },
    );
    let serial = SERIAL_COUNTER.next_serial();
    pointer.button(
        &mut compositor.state,
        &pointer::ButtonEvent {
            button: BTN_RIGHT,
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
    let Some(wm) = backend.window_manager.as_ref() else { return };
    let Some(window) = wm.windows.get(&window_id) else { return };
    let wl_surface = window.surface_kind.wl_surface().clone();

    let compositor = &mut backend.compositor;
    let serial = SERIAL_COUNTER.next_serial();
    let time = compositor.start_time.elapsed().as_millis() as u32;

    // Set keyboard focus only if this surface doesn't already have it.
    // Calling set_focus on every key event floods the client with keymap data and causes ANR.
    if let Some(kb) = &compositor.keyboard {
        let needs_focus = kb.current_focus().as_ref() != Some(&wl_surface);
        if needs_focus {
            kb.set_focus(&mut compositor.state, Some(wl_surface.clone()), serial);
        }
    }

    let Some(linux_keycode) = super::keymap::android_keycode_to_smithay(key_code) else {
        log::debug!("Unmapped Android keycode: {}", key_code);
        return;
    };
    let key_state = if action == ACTION_DOWN {
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
