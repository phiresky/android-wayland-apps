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
                        window.native_window = Some(native_window);
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
                        // Only set needs_redraw for non-Vulkan windows.
                        // For Vulkan, needs_redraw is set by client commits only —
                        // re-blitting the old committed buffer shows stale content.
                        if window.vk_surface.is_none() {
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
                        window.egl_surface = None;
                        window.native_window = None;
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
                        if window.native_window.is_none() {
                            tracing::info!("Window {} Activity gone, will relaunch if client refuses close", window_id);
                            window.close_pending_since = Some(Instant::now());
                            window.activity_launched = false;
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
            WindowEvent::ImeComposing { window_id, text } => {
                handle_ime_composing(backend, window_id, text);
            }
            WindowEvent::ImeCommit { window_id, text } => {
                handle_ime_commit(backend, window_id, text);
            }
            WindowEvent::ImeDelete { window_id, before, after, text } => {
                handle_ime_delete(backend, window_id, before, after, &text);
            }
            WindowEvent::ImeRecompose { window_id, text } => {
                handle_ime_recompose(backend, window_id, text);
            }
        }
    }
}

/// Render each Activity window's toplevel to its EGL surface.
fn render_activity_windows(backend: &mut WaylandBackend) {
    let time = backend.compositor.start_time.elapsed().as_millis() as u32;

    // Send frame callbacks for windows that can't be rendered yet (no EGL/VK surface).
    // Without this, EGL clients (e.g. Factorio via llvmpipe) block forever in
    // eglSwapBuffers waiting for a frame callback that never comes.
    if let Some(wm) = backend.window_manager.as_ref() {
        for (_, window) in &wm.windows {
            if window.egl_surface.is_none() && window.vk_surface.is_none() {
                send_frames_surface_tree(window.surface_kind.wl_surface(), time);
            }
        }
    }

    // Mark windows as needing redraw based on committed surfaces.
    let committed: Vec<WlSurface> = backend.compositor.state.committed_surfaces.drain(..).collect();
    if let Some(wm) = backend.window_manager.as_mut() {
        for surface in &committed {
            for (_, window) in wm.windows.iter_mut() {
                if window.surface_kind.wl_surface() == surface {
                    window.needs_redraw = true;
                }
            }
        }
    }

    let window_ids: Vec<u32> = backend
        .window_manager
        .as_ref()
        .map(|wm| {
            wm.windows
                .iter()
                .filter(|(_, w)| (w.egl_surface.is_some() || w.vk_surface.is_some() || w.native_window.is_some()) && w.size.w > 0 && w.size.h > 0)
                .map(|(id, _)| *id)
                .collect()
        })
        .unwrap_or_default();

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

        // Try Vulkan zero-copy blit for dmabuf surfaces
        let vk_rendered = {
            use smithay::wayland::compositor::with_states;
            use smithay::wayland::dmabuf::get_dmabuf;
            use smithay::backend::allocator::Buffer;
            use smithay::backend::renderer::utils::RendererSurfaceState;
            use crate::android::window_manager::RenderMode;

            // Set render mode from global toggle on first commit
            if let Some(wm) = backend.window_manager.as_mut() {
                if let Some(window) = wm.windows.get_mut(&window_id) {
                    if window.render_mode.is_none() {
                        window.render_mode = Some(if crate::android::window_manager::use_vulkan_rendering() {
                            RenderMode::Vulkan
                        } else {
                            RenderMode::Gles
                        });
                    }
                }
            }

            let render_mode = backend.window_manager.as_ref()
                .and_then(|wm| wm.windows.get(&window_id))
                .and_then(|w| w.render_mode)
                .unwrap_or(RenderMode::Vulkan);

            let mut done = false;
            if render_mode == RenderMode::Vulkan {
            if let Some(ref vk) = backend.vk_renderer {
                // Check if surface has a dmabuf buffer
                let dmabuf = with_states(&wl_surface, |states| {
                    type RssType = std::sync::Mutex<RendererSurfaceState>;
                    states.data_map.get::<RssType>()
                        .and_then(|rss| {
                            let guard = rss.lock().ok()?;
                            let buf = guard.buffer()?;
                            get_dmabuf(buf).ok().cloned()
                        })
                });

                if let Some(dmabuf) = dmabuf {
                    let dmabuf_sz = dmabuf.size();
                    let buf_w = dmabuf_sz.w as u32;
                    let buf_h = dmabuf_sz.h as u32;
                    let fmt = dmabuf.format();
                    let vk_fmt = crate::android::backend::vulkan_renderer::VulkanRenderer::fourcc_to_vk_format(fmt.code as u32);

                    // Check if we need to (re)create the Vulkan swapchain:
                    // - No surface yet, or
                    // - Client buffer size changed (resize)
                    let needs_vk_surface = backend.window_manager.as_ref()
                        .and_then(|wm| wm.windows.get(&window_id))
                        .map(|w| {
                            if w.vk_surface.is_none() && w.native_window.is_some() {
                                return true;
                            }
                            // Recreate if buffer size changed
                            if let Some(ref vks) = w.vk_surface {
                                if vks.extent.width != buf_w || vks.extent.height != buf_h {
                                    return true;
                                }
                            }
                            false
                        })
                        .unwrap_or(false);

                    if needs_vk_surface {
                        if let Some(wm) = backend.window_manager.as_mut() {
                            if let Some(window) = wm.windows.get_mut(&window_id) {
                                if window.egl_surface.is_some() {
                                    tracing::info!("Destroying EGL surface for Vulkan takeover on window_id={}", window_id);
                                    window.egl_surface = None;
                                }
                                window.needs_redraw = true;

                                if let Some(native_window) = window.native_window {
                                    let result = if let Some(ref old_vk) = window.vk_surface {
                                        // Resize: chain new swapchain from old (no surface destroy)
                                        vk.resize_swapchain(old_vk, native_window, buf_w, buf_h, vk_fmt)
                                    } else {
                                        // First creation
                                        vk.create_window_surface(native_window, buf_w, buf_h, vk_fmt)
                                    };
                                    match result {
                                        Ok(vk_surface) => {
                                            tracing::info!("Vulkan surface for window_id={} at {}x{}", window_id, buf_w, buf_h);
                                            // Flush SurfaceFlinger's initial buffer by presenting
                                            // black frames to all swapchain images. Without this,
                                            // the SurfaceView's initial buffer stays in the
                                            // consumer queue and strobes against Vulkan frames.
                                            for _ in 0..vk_surface.images.len() {
                                                let _ = vk.present_black(&vk_surface);
                                            }
                                            window.vk_surface = Some(vk_surface);
                                        }
                                        Err(e) => {
                                            tracing::error!("Vulkan surface creation failed: {e}");
                                            window.vk_surface = None;
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // Now try to blit if we have a vk_surface
                    if let Some(ref wm) = backend.window_manager {
                        if let Some(window) = wm.windows.get(&window_id) {
                            if let Some(ref vk_surface) = window.vk_surface {
                                let sz = dmabuf.size();
                                let fd = dmabuf.handles().next();
                                let stride = dmabuf.strides().next().unwrap_or(sz.w as u32 * 4);
                                if let Some(fd) = fd {
                                    use std::os::unix::io::AsRawFd;
                                    let raw_fd = fd.as_raw_fd();
                                    tracing::info!("[vk-render] fd={} needs_redraw={}", raw_fd, window.needs_redraw);
                                    if !window.needs_redraw {
                                        done = true;
                                    } else {
                                    tracing::debug!("[vk-render] blit fd={} needs_redraw=true", raw_fd);
                                    match vk.get_or_import_dmabuf(raw_fd, sz.w as u32, sz.h as u32, stride, vk_fmt) {
                                        Ok(imported) => {
                                            match vk.blit_dmabuf_to_swapchain(&imported, vk_surface) {
                                                Ok(()) => {
                                                    done = true;
                                                    if let Some(wm) = backend.window_manager.as_mut() {
                                                        if let Some(w) = wm.windows.get_mut(&window_id) {
                                                            w.last_render_method = "VK dmabuf";
                                                            w.last_buffer_size = Some((sz.w as u32, sz.h as u32));
                                                            w.needs_redraw = false;
                                                        }
                                                    }
                                                }
                                                Err(e) => tracing::warn!("Vulkan blit failed: {e}"),
                                            }
                                        }
                                        Err(e) => tracing::warn!("Vulkan dmabuf import failed: {e}"),
                                    }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            } // render_mode == Vulkan
            done
        };

        if vk_rendered {
            // Vulkan handled this window — send frame callbacks and skip GLES
            send_frames_surface_tree(&wl_surface, time);
            continue;
        }

        // Render in a scoped block so borrows are released before submit
        {
            let Some(wm) = backend.window_manager.as_mut() else {
                continue;
            };
            let Some(window) = wm.windows.get_mut(&window_id) else {
                continue;
            };
            // Lazy EGL surface creation: only create when:
            // 1. No VK surface exists (client doesn't use dmabuf)
            // 2. Window has had a commit (needs_redraw) — so we know it's wl_shm
            // Creating EGL on a window that will later use Vulkan causes a stale
            // EGL frame to persist in SurfaceFlinger's queue, causing strobe.
            if window.egl_surface.is_none() && window.vk_surface.is_none()
                && window.native_window.is_some() && window.needs_redraw {
                if let Some(handle) = wm.get_native_handle(window_id) {
                    if let Some(surface) = backend.renderer.as_ref()
                        .and_then(|r| r.create_surface_for_native_window(handle).ok()) {
                        tracing::info!("Lazy-created EGL surface for window_id={}", window_id);
                        if let Some(w) = wm.windows.get_mut(&window_id) {
                            w.egl_surface = Some(surface);
                        }
                    }
                }
            }
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
                tracing::error!("Failed to bind surface for window_id={}", window_id);
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
                tracing::error!("Failed to begin render for window_id={}", window_id);
                continue;
            };

            if let Err(e) = frame.clear(Color32F::new(0.0, 0.0, 0.0, 1.0), &[damage]) {
                tracing::warn!("frame.clear failed for window_id={}: {e:?}", window_id);
            }
            if let Err(e) = draw_render_elements(&mut frame, scale, &elements, &[damage]) {
                tracing::warn!("draw_render_elements failed for window_id={}: {e:?}", window_id);
            }
            if let Err(e) = frame.finish() {
                tracing::warn!("frame.finish failed for window_id={}: {e:?}", window_id);
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
        // Count rendered frame for FPS tracking + mark GLES render method.
        if let Some(wm) = backend.window_manager.as_mut()
            && let Some(window) = wm.windows.get_mut(&window_id)
        {
            window.frame_count += 1;
            if window.last_render_method != "VK dmabuf" {
                window.last_render_method = "GLES shm";
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
                window.frame_count as f64 / elapsed_secs
            } else {
                0.0
            };
            window.frame_count = 0;
            let display_name = if !title.is_empty() { &title } else if !app_id.is_empty() { &app_id } else { "(untitled)" };
            let phys = format!("{}x{}", window.size.w, window.size.h);
            let logical_w = (window.size.w as f64 / scale).round() as i32;
            let logical_h = (window.size.h as f64 / scale).round() as i32;
            let buf_str = buf_size.map(|s| format!("{}x{}", s.w, s.h)).unwrap_or_else(|| "?".into());
            let pref_str = window.preferred_size.map(|p| format!("{}x{}", p.w, p.h)).unwrap_or_else(|| "-".into());
            let surface_type = if window.vk_surface.is_some() { "VK" } else if window.egl_surface.is_some() { "EGL" } else { "-" };
            let frac = if has_frac_scale { "frac" } else { "1x" };
            info.push_str(&format!(
                "  [{}] {} | {}  phys={}  log={}x{}  buf={}  pref={}  {} {} {:.0}fps\n",
                id, display_name, window.last_render_method,
                phys, logical_w, logical_h, buf_str, pref_str,
                surface_type, frac, fps,
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
                // Don't relaunch while a close request is pending — give the
                // client time to process it. Relaunch after 500ms (client refused).
                .filter(|(_, w)| w.close_pending_since.map_or(true,
                    |t| t.elapsed() > std::time::Duration::from_millis(500)))
                .filter_map(|(&id, w)| {
                    // Read the client's committed geometry, or fall back to buffer size.
                    // Many simple apps (vkcube, eglgears) don't set XDG geometry.
                    let geo_size = match &w.surface_kind {
                        SurfaceKind::Toplevel(_) => {
                            wl_compositor::with_states(w.surface_kind.wl_surface(), |states| {
                                let geo = states.cached_state.get::<SurfaceCachedState>()
                                    .current()
                                    .geometry
                                    .map(|g| g.size);
                                if geo.is_some() {
                                    return geo;
                                }
                                // No geometry set — use buffer size as fallback
                                type RssType = std::sync::Mutex<smithay::backend::renderer::utils::RendererSurfaceState>;
                                states.data_map.get::<RssType>()
                                    .and_then(|rss| rss.lock().ok())
                                    .and_then(|guard| guard.buffer_size())
                            })
                        }
                        SurfaceKind::Layer(_) => None,
                    };
                    let bounds = geo_size.map(|s| {
                        ((s.w as f64 * scale).round() as i32,
                         (s.h as f64 * scale).round() as i32 + DEX_TITLE_BAR_HEIGHT)
                    }).filter(|&(w, h)| w > 0 && h > 0);

                    tracing::info!("launch_pending: window_id={id} geo_size={geo_size:?} bounds={bounds:?} elapsed={}ms",
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
        tracing::debug!("Unmapped Android keycode: {}", key_code);
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

// ============================================================
// IME text input handlers (composing / commit / delete)
// ============================================================

/// Ensure the keyboard is focused on the given window's surface.
fn ensure_ime_focus(backend: &mut WaylandBackend, window_id: u32) {
    let Some(wm) = backend.window_manager.as_ref() else { return };
    let Some(window) = wm.windows.get(&window_id) else { return };
    let wl_surface = window.surface_kind.wl_surface().clone();
    let compositor = &mut backend.compositor;
    if let Some(kb) = &compositor.keyboard {
        if kb.current_focus().as_ref() != Some(&wl_surface) {
            let serial = SERIAL_COUNTER.next_serial();
            kb.set_focus(&mut compositor.state, Some(wl_surface), serial);
        }
    }
}

/// Handle composing (preedit) text from Android IME.
fn handle_ime_composing(backend: &mut WaylandBackend, window_id: u32, text: String) {
    ensure_ime_focus(backend, window_id);

    if backend.compositor.state.text_input_state.is_active() {
        // text_input_v3 path: send preedit_string + done
        backend.compositor.state.text_input_state.send_preedit(&text);
        backend.compositor.state.text_input_state.composing_text = text;
    } else {
        // Key-event fallback: diff against previous composing text
        let old = std::mem::replace(
            &mut backend.compositor.state.text_input_state.composing_text,
            text.clone(),
        );
        let common = common_prefix_byte_len(&old, &text);
        let del = old[common..].chars().count();
        let ins = &text[common..];
        send_ime_key_events(backend, del, 0, ins);
    }
}

/// Handle committed text from Android IME.
fn handle_ime_commit(backend: &mut WaylandBackend, window_id: u32, text: String) {
    ensure_ime_focus(backend, window_id);

    if backend.compositor.state.text_input_state.is_active() {
        // text_input_v3 path: send commit_string + done
        backend.compositor.state.text_input_state.send_commit(&text);
        backend.compositor.state.text_input_state.composing_text.clear();
    } else {
        // Key-event fallback: diff against composing text, then clear
        let old = std::mem::take(&mut backend.compositor.state.text_input_state.composing_text);
        let common = common_prefix_byte_len(&old, &text);
        let del = old[common..].chars().count();
        let ins = &text[common..];
        send_ime_key_events(backend, del, 0, ins);
    }
}

/// Handle delete surrounding text from Android IME (e.g. backspace).
/// `deleted_text` is the actual text being deleted (from the Editable), used for
/// correct UTF-8 byte counts in text_input_v3 (emoji = 4 bytes, not 1).
fn handle_ime_delete(backend: &mut WaylandBackend, window_id: u32, before: i32, after: i32, deleted_text: &str) {
    ensure_ime_focus(backend, window_id);

    if backend.compositor.state.text_input_state.is_active() {
        // text_input_v3 path: use actual UTF-8 byte length for correct emoji handling
        let before_bytes = if deleted_text.is_empty() { before as u32 } else { deleted_text.len() as u32 };
        backend.compositor.state.text_input_state.send_delete(before_bytes, after as u32);
    } else {
        // Key-event fallback: use character counts (one backspace per char)
        send_ime_key_events(backend, before as usize, after as usize, "");
    }
}

/// Handle setComposingRegion: already-committed text is being turned back into composing.
/// For text_input_v3: delete the committed text and re-show as preedit.
/// For key-event fallback: just update tracking (text is already on screen).
fn handle_ime_recompose(backend: &mut WaylandBackend, window_id: u32, text: String) {
    ensure_ime_focus(backend, window_id);

    if backend.compositor.state.text_input_state.is_active() {
        // text_input_v3: atomically delete committed text and show as preedit
        backend.compositor.state.text_input_state.send_recompose(&text);
    }
    // Both paths: update composing text tracking (no key events needed)
    backend.compositor.state.text_input_state.composing_text = text;
}

/// Find the byte length of the common character prefix between two strings.
fn common_prefix_byte_len(a: &str, b: &str) -> usize {
    a.chars()
        .zip(b.chars())
        .take_while(|(ca, cb)| ca == cb)
        .map(|(c, _)| c.len_utf8())
        .sum()
}

/// Send synthetic wl_keyboard events: backspaces, forward deletes, then characters.
/// Used as fallback when text_input_v3 is not active.
fn send_ime_key_events(
    backend: &mut WaylandBackend,
    delete_before: usize,
    delete_after: usize,
    text: &str,
) {
    let compositor = &mut backend.compositor;
    let time = compositor.start_time.elapsed().as_millis() as u32;

    let Some(kb) = &compositor.keyboard else { return };

    const BACKSPACE: u32 = 14 + 8;
    for _ in 0..delete_before {
        let serial = SERIAL_COUNTER.next_serial();
        kb.input::<(), _>(&mut compositor.state, BACKSPACE.into(),
            smithay::backend::input::KeyState::Pressed, serial, time,
            |_, _, _| FilterResult::Forward);
        let serial = SERIAL_COUNTER.next_serial();
        kb.input::<(), _>(&mut compositor.state, BACKSPACE.into(),
            smithay::backend::input::KeyState::Released, serial, time,
            |_, _, _| FilterResult::Forward);
    }

    const DELETE: u32 = 111 + 8;
    for _ in 0..delete_after {
        let serial = SERIAL_COUNTER.next_serial();
        kb.input::<(), _>(&mut compositor.state, DELETE.into(),
            smithay::backend::input::KeyState::Pressed, serial, time,
            |_, _, _| FilterResult::Forward);
        let serial = SERIAL_COUNTER.next_serial();
        kb.input::<(), _>(&mut compositor.state, DELETE.into(),
            smithay::backend::input::KeyState::Released, serial, time,
            |_, _, _| FilterResult::Forward);
    }

    const SHIFT: u32 = 42 + 8;
    for ch in text.chars() {
        if let Some((evdev, shift)) = super::keymap::char_to_evdev_key(ch) {
            let code = evdev + 8;
            if shift {
                let serial = SERIAL_COUNTER.next_serial();
                kb.input::<(), _>(&mut compositor.state, SHIFT.into(),
                    smithay::backend::input::KeyState::Pressed, serial, time,
                    |_, _, _| FilterResult::Forward);
            }
            let serial = SERIAL_COUNTER.next_serial();
            kb.input::<(), _>(&mut compositor.state, code.into(),
                smithay::backend::input::KeyState::Pressed, serial, time,
                |_, _, _| FilterResult::Forward);
            let serial = SERIAL_COUNTER.next_serial();
            kb.input::<(), _>(&mut compositor.state, code.into(),
                smithay::backend::input::KeyState::Released, serial, time,
                |_, _, _| FilterResult::Forward);
            if shift {
                let serial = SERIAL_COUNTER.next_serial();
                kb.input::<(), _>(&mut compositor.state, SHIFT.into(),
                    smithay::backend::input::KeyState::Released, serial, time,
                    |_, _, _| FilterResult::Forward);
            }
        } else {
            tracing::debug!("IME: unmapped char {:?} (U+{:04X})", ch, ch as u32);
        }
    }
}

