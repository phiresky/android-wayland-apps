use crate::android::backend::WaylandBackend;
use crate::android::compositor::ClientState;
use crate::android::window_manager::{SurfaceKind, WindowEvent};
use smithay::wayland::compositor as wl_compositor;
use smithay::wayland::shell::wlr_layer::LayerSurface;
use smithay::wayland::shell::xdg::{SurfaceCachedState, ToplevelSurface, XdgToplevelSurfaceData};
use std::sync::Arc;
use std::time::Instant;

const WINDOW_ACTIVITY_CLASS: &str = "io.github.phiresky.wayland_android.WaylandWindowActivity";

/// Height of the Samsung DeX window title bar in physical pixels.
/// setLaunchBounds specifies outer window bounds (including chrome),
/// so we add this to the content height to get the correct content area.
const DEX_TITLE_BAR_HEIGHT: i32 = 70;

/// One iteration of the compositor loop: dispatch protocol, render, update status.
pub fn compositor_tick(backend: &mut WaylandBackend) {
    dispatch_wayland(backend);
    super::render::render_activity_windows(backend);
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
                    tracing::info!("{kind} destroyed, finishing Activity window_id={window_id}");
                    if let Err(e) = finish_activity(window_id) {
                        tracing::error!("Failed to finish Activity for window_id={window_id}: {e}");
                    }
                    wm.remove_window(window_id);
                    // Clear dmabuf cache — stale fd→GPU memory mappings cause
                    // strobing when the kernel recycles fd numbers for new clients.
                    if let Some(ref vk) = backend.vk_renderer {
                        vk.clear_dmabuf_cache();
                    }
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
                Err(e) => tracing::error!("Failed to insert client: {:?}", e),
            }
        }
        Ok(None) => {}
        Err(e) => tracing::error!("Failed to accept listener: {:?}", e),
    }

    if let Err(e) = backend
        .compositor
        .display
        .dispatch_clients(&mut backend.compositor.state)
    {
        tracing::error!("Failed to dispatch clients: {:?}", e);
    }

    backend.compositor.clients.retain(|c| {
        c.get_data::<ClientState>().is_some_and(|s| s.is_alive())
    });
    // Process soft keyboard show/hide from text_input_v3.
    if let Some((visible, android_input_type)) = backend.compositor.state.soft_keyboard_request.take() {
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
            if let Err(e) = set_soft_keyboard_visible(window_id, visible, android_input_type) {
                tracing::error!("Failed to set soft keyboard visibility: {e}");
            }
        }
    }

    if let Err(e) = backend
        .compositor
        .display
        .flush_clients()
    {
        tracing::error!("Failed to flush clients: {:?}", e);
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
                // Store the native window pointer
                if let Some(wm) = backend.window_manager.as_mut()
                    && let Some(window) = wm.windows.get_mut(&window_id) {
                        window.render.native_window = Some(native_window);
                    }
                // EGL surface is created lazily on first GLES render (not here).
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
                        // Only set needs_redraw for non-AHB windows.
                        // For AHB/dmabuf, needs_redraw is set by client commits only —
                        // re-blitting the old committed buffer shows stale content.
                        if window.render.ahb_surface.is_none() {
                            window.needs_redraw = true;
                        }
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
                                tracing::info!("SurfaceChanged window_id={window_id}: physical={width}x{height} -> configure logical={logical_w}x{logical_h} (preferred={:?}, scale={scale})",
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
                        tracing::info!("Window {} resized to {}x{} (logical {}x{}, scale {})",
                            window_id, width, height, logical_w, logical_h, scale);
                    }
            }
            WindowEvent::SurfaceDestroyed { window_id } => {
                if let Some(wm) = backend.window_manager.as_mut()
                    && let Some(window) = wm.windows.get_mut(&window_id) {
                        if let Some(ref ahb) = window.render.ahb_surface {
                            if let Some(ref vk) = backend.vk_renderer {
                                if let Some(ref target) = ahb.ahb_target {
                                    vk.destroy_ahb_target(target);
                                }
                            }
                        }
                        window.render.ahb_surface = None;
                        window.render.egl_surface = None;
                        window.render.native_window = None;
                        tracing::info!("Surface destroyed for window_id={}", window_id);
                    }
            }
            WindowEvent::CloseRequested { window_id } => {
                // User requested close (back button, DeX X). Send XDG close to client.
                // The client may refuse (e.g. "save changes?" dialog).
                // The Activity only finishes when the client destroys its surface.
                if let Some(wm) = backend.window_manager.as_mut()
                    && let Some(window) = wm.windows.get_mut(&window_id) {
                        window.surface_kind.send_close();
                        // If the Activity was already destroyed (DeX X bypasses finish()),
                        // mark for delayed relaunch — the client may refuse to close.
                        if window.render.native_window.is_none() {
                            tracing::info!("Window {} Activity gone, will relaunch if client refuses close", window_id);
                            window.lifecycle.close_pending_since = Some(Instant::now());
                            window.lifecycle.activity_launched = false;
                        }
                    }
            }
            WindowEvent::WindowClosed { window_id, is_finishing } => {
                if is_finishing {
                    // Activity was actually destroyed (compositor-initiated via finishByWindowId).
                    // Clean up if the window still exists (safety net).
                    if let Some(wm) = backend.window_manager.as_mut() {
                        wm.remove_window(window_id);
                    }
                } else {
                    tracing::info!("Window {} destroyed by Android (config change), keeping toplevel alive", window_id);
                }
            }
            WindowEvent::Touch {
                window_id,
                action,
                x,
                y,
            } => {
                super::input::handle_activity_touch(backend, window_id, action, x, y);
            }
            WindowEvent::Key {
                window_id,
                key_code,
                action,
                meta_state,
            } => {
                super::input::handle_activity_key(backend, window_id, key_code, action, meta_state);
            }
            WindowEvent::RightClick {
                window_id,
                x,
                y,
            } => {
                super::input::handle_activity_right_click(backend, window_id, x, y);
            }
            WindowEvent::ImeComposing { window_id, text } => {
                super::ime::handle_ime_composing(backend, window_id, text);
            }
            WindowEvent::ImeCommit { window_id, text } => {
                super::ime::handle_ime_commit(backend, window_id, text);
            }
            WindowEvent::ImeDelete { window_id, before, after, text } => {
                super::ime::handle_ime_delete(backend, window_id, before, after, &text);
            }
            WindowEvent::ImeRecompose { window_id, text } => {
                super::ime::handle_ime_recompose(backend, window_id, text);
            }
            WindowEvent::PortalRequest(request) => {
                crate::android::portal::handle_portal_request(&request);
            }
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

    let scale = backend.scale_factor;
    let mut info = format!("Clients: {}  Toplevels: {}  Scale: {:.2}\n", num_clients, num_toplevels, scale);

    if let Some(wm) = backend.window_manager.as_mut() {
        for (id, window) in &mut wm.windows {
            let wl_surface = window.surface_kind.wl_surface();
            let (title, app_id, buf_size, has_frac_scale) = wl_compositor::with_states(wl_surface, |states| {
                let title = states.data_map
                    .get::<XdgToplevelSurfaceData>()
                    .and_then(|data| data.lock().ok())
                    .and_then(|attrs| attrs.title.clone())
                    .unwrap_or_default();
                let app_id = states.data_map
                    .get::<XdgToplevelSurfaceData>()
                    .and_then(|data| data.lock().ok())
                    .and_then(|attrs| attrs.app_id.clone())
                    .unwrap_or_default();
                // Get buffer logical size from renderer surface state
                type RssType = std::sync::Mutex<smithay::backend::renderer::utils::RendererSurfaceState>;
                let buf_size = states.data_map.get::<RssType>()
                    .and_then(|rss| rss.lock().ok())
                    .and_then(|guard| guard.buffer_size());
                // Check if client bound fractional_scale
                let has_frac = states.data_map
                    .get::<smithay::wayland::fractional_scale::FractionalScaleStateUserData>()
                    .is_some();
                (title, app_id, buf_size, has_frac)
            });
            let fps = if elapsed_secs > 0.0 {
                window.metrics.frame_count as f64 / elapsed_secs
            } else {
                0.0
            };
            window.metrics.frame_count = 0;
            let display_name = if !title.is_empty() { &title } else if !app_id.is_empty() { &app_id } else { "(untitled)" };
            let phys = format!("{}x{}", window.size.w, window.size.h);
            let logical_w = (window.size.w as f64 / scale).round() as i32;
            let logical_h = (window.size.h as f64 / scale).round() as i32;
            let buf_str = buf_size.map(|s| format!("{}x{}", s.w, s.h)).unwrap_or_else(|| "?".into());
            let pref_str = window.preferred_size.map(|p| format!("{}x{}", p.w, p.h)).unwrap_or_else(|| "-".into());
            let surface_type = if window.render.ahb_surface.is_some() { "AHB" } else if window.render.egl_surface.is_some() { "EGL" } else { "-" };
            let frac = if has_frac_scale { "frac" } else { "1x" };
            info.push_str(&format!(
                "  [{}] {} | {}  phys={}  log={}x{}  buf={}  pref={}  {} {} {:.0}fps  {}us\n",
                id, display_name, window.metrics.last_render_method,
                phys, logical_w, logical_h, buf_str, pref_str,
                surface_type, frac, fps, window.metrics.last_frame_us,
            ));
        }
    }

    tracing::debug!("Status: {}", info.trim());
    if let Err(e) = send_status_jni(&info) {
        tracing::error!("Status overlay JNI call failed: {e}");
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
    android_input_type: i32,
) -> Result<(), jni::errors::Error> {
    crate::android::utils::jni_context::with_jni(|env, activity| {
        let class = crate::android::utils::jni_context::load_class(
            env, activity, WINDOW_ACTIVITY_CLASS,
        )?;
        env.call_static_method(
            class,
            "setSoftKeyboardVisible",
            "(IZI)V",
            &[
                jni::objects::JValue::Int(window_id as i32),
                jni::objects::JValue::Bool(u8::from(visible)),
                jni::objects::JValue::Int(android_input_type),
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
                .filter(|(_, w)| !w.lifecycle.activity_launched)
                // Don't relaunch while a close request is pending — give the
                // client time to process it. Relaunch after 500ms (client refused).
                .filter(|(_, w)| w.lifecycle.close_pending_since.map_or(true,
                    |t| t.elapsed() > std::time::Duration::from_millis(500)))
                .filter_map(|(&id, w)| {
                    // Read the client's committed geometry (logical, needs scaling)
                    // or fall back to buffer pixel dimensions (physical, no scaling).
                    let (geo_size, bounds) = match &w.surface_kind {
                        SurfaceKind::Toplevel(_) => {
                            wl_compositor::with_states(w.surface_kind.wl_surface(), |states| {
                                // Try XDG geometry first (logical coordinates)
                                let geo = states.cached_state.get::<SurfaceCachedState>()
                                    .current()
                                    .geometry
                                    .map(|g| g.size);
                                if let Some(g) = geo {
                                    let bounds = (
                                        (g.w as f64 * scale).round() as i32,
                                        (g.h as f64 * scale).round() as i32 + DEX_TITLE_BAR_HEIGHT,
                                    );
                                    return (Some(g), Some(bounds));
                                }
                                // No geometry — use buffer pixel size directly as bounds.
                                // Don't scale: the buffer IS the physical size the client chose.
                                type RssType = std::sync::Mutex<smithay::backend::renderer::utils::RendererSurfaceState>;
                                let buf = states.data_map.get::<RssType>()
                                    .and_then(|rss| rss.lock().ok())
                                    .and_then(|guard| guard.buffer_size());
                                match buf {
                                    Some(b) => (Some(b), Some((b.w, b.h + DEX_TITLE_BAR_HEIGHT))),
                                    None => (None, None),
                                }
                            })
                        }
                        SurfaceKind::Layer(_) => (None, None),
                    };
                    let bounds = bounds.filter(|&(w, h)| w > 0 && h > 0);

                    tracing::info!("launch_pending: window_id={id} geo_size={geo_size:?} bounds={bounds:?} elapsed={}ms",
                        w.lifecycle.created_time.elapsed().as_millis());
                    if bounds.is_some() {
                        // Client has committed with geometry — launch with bounds.
                        Some((id, bounds, geo_size))
                    } else if w.lifecycle.created_time.elapsed() > std::time::Duration::from_millis(500) {
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
