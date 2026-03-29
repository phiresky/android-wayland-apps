use crate::android::backend::WaylandBackend;
use crate::android::compositor::send_frames_surface_tree;
use crate::android::window_manager::SurfaceKind;
use smithay::backend::renderer::element::surface::{
    render_elements_from_surface_tree, WaylandSurfaceRenderElement,
};
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::utils::draw_render_elements;
use smithay::backend::renderer::{Color32F, Frame, Renderer};
use smithay::desktop::PopupManager;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Rectangle, Transform};
use smithay::wayland::compositor as wl_compositor;
use smithay::wayland::shell::xdg::SurfaceCachedState;
use std::time::Instant;

/// Render each Activity window's toplevel to its EGL surface.
pub(crate) fn render_activity_windows(backend: &mut WaylandBackend) {
    let time = backend.compositor.start_time.elapsed().as_millis() as u32;

    // Send frame callbacks for windows that can't be rendered yet (no EGL/VK surface).
    // Without this, EGL clients (e.g. Factorio via llvmpipe) block forever in
    // eglSwapBuffers waiting for a frame callback that never comes.
    if let Some(wm) = backend.window_manager.as_ref() {
        for (_, window) in &wm.windows {
            if window.render.egl_surface.is_none() && window.render.ahb_surface.is_none() {
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
                .filter(|(_, w)| (w.render.egl_surface.is_some() || w.render.ahb_surface.is_some() || w.render.native_window.is_some()) && w.size.w > 0 && w.size.h > 0)
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

        // Flush and unbind EGL context before Vulkan operations. On Qualcomm,
        // EGL and Vulkan share the GPU via KGSL — interleaved GPU commands
        // cause buffer corruption (horizontal stripes).
        // glFinish() waits for all pending GLES ops to complete on GPU.
        // eglReleaseThread() fully releases EGL/KGSL state from the thread.
        if let Some(cr) = backend.renderer.as_mut() {
            let _ = cr.renderer.with_context(|gl| {
                unsafe { gl.Finish(); }
            });
            let _ = cr.renderer.egl_context().unbind();
            unsafe { smithay::backend::egl::ffi::egl::ReleaseThread(); }
        }

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
                    if window.render.render_mode.is_none() {
                        window.render.render_mode = Some(if crate::android::window_manager::use_vulkan_rendering() {
                            RenderMode::Vulkan
                        } else {
                            RenderMode::Gles
                        });
                    }
                }
            }

            let render_mode = backend.window_manager.as_ref()
                .and_then(|wm| wm.windows.get(&window_id))
                .and_then(|w| w.render.render_mode)
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

                if dmabuf.is_none() {
                    // Log shm buffer details when dmabuf detection fails
                    let is_shm = with_states(&wl_surface, |states| {
                        type RssType = std::sync::Mutex<RendererSurfaceState>;
                        states.data_map.get::<RssType>()
                            .and_then(|rss| {
                                let guard = rss.lock().ok()?;
                                let buf = guard.buffer()?;
                                smithay::wayland::shm::with_buffer_contents(&buf, |_, _, data| {
                                    (data.width, data.height, data.format)
                                }).ok()
                            })
                    });
                    if let Some((w, h, fmt)) = is_shm {
                        tracing::warn!("[buffer-type] window_id={} BUFFER=shm ({}x{} fmt={:?}) — not dmabuf! Firefox/Zink should use zwp_linux_dmabuf_v1", window_id, w, h, fmt);
                    }
                }
                tracing::debug!("[vk-dmabuf] window_id={} dmabuf={}", window_id, dmabuf.is_some());
                if let Some(dmabuf) = dmabuf {
                    let dmabuf_sz = dmabuf.size();
                    let buf_w = dmabuf_sz.w as u32;
                    let buf_h = dmabuf_sz.h as u32;
                    let fmt = dmabuf.format();
                    let vk_fmt = crate::android::backend::vulkan_renderer::VulkanRenderer::fourcc_to_vk_format(fmt.code as u32);

                    // === Zero-copy direct AHB present ===
                    // If this dmabuf is a compositor-allocated AHB, skip the
                    // Vulkan import+blit entirely and present the AHB directly
                    // via ASurfaceTransaction. This is the optimal zero-GPU-copy path.
                    #[cfg(feature = "zero-copy")]
                    let gbm_lookup = {
                        let size_stable = backend.window_manager.as_ref()
                            .and_then(|wm| wm.windows.get(&window_id))
                            .and_then(|w| w.metrics.last_buffer_size)
                            .map(|(lw, lh)| lw == buf_w && lh == buf_h)
                            .unwrap_or(false);

                        if size_stable && crate::android::window_manager::zero_copy_enabled() {
                            backend.gbm_state.as_ref()
                                .and_then(|s| s.lock().ok())
                                .and_then(|g| g.tracker.lookup(&dmabuf).map(|b| b.ahb.clone()))
                        } else {
                            None
                        }
                    };
                    #[cfg(not(feature = "zero-copy"))]
                    let gbm_lookup: Option<std::sync::Arc<crate::android::backend::surface_transaction::HardwareBuffer>> = None;
                    if let Some(ahb_arc) = gbm_lookup {
                        // Ensure ASurfaceControl exists for this window.
                        let has_sc = backend.window_manager.as_ref()
                            .and_then(|wm| wm.windows.get(&window_id))
                            .map(|w| w.render.ahb_surface.is_some())
                            .unwrap_or(false);
                        if !has_sc {
                            if let Some(wm) = backend.window_manager.as_mut() {
                                if let Some(window) = wm.windows.get_mut(&window_id) {
                                    if let Some(native_window) = window.render.native_window {
                                        if window.render.egl_surface.is_some() {
                                            tracing::info!("Destroying EGL for zero-copy AHB window_id={}", window_id);
                                            window.render.egl_surface = None;
                                        }
                                        let sc = crate::android::backend::surface_transaction::SurfaceControlHandle::from_window(
                                            native_window, &format!("wl-zc-{window_id}"));
                                        if let Some(sc) = sc {
                                            crate::android::backend::surface_transaction::set_visible(&sc);
                                            // Zero-copy: no AhbTarget needed (no blit).
                                            window.render.ahb_surface = Some(crate::android::window_manager::AhbWindowSurface {
                                                surface_control: sc,
                                                ahb_target: None,
                                                frame_in_flight: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                                            });
                                            window.needs_redraw = true;
                                            tracing::info!("Zero-copy AHB surface created for window_id={} at {}x{}", window_id, buf_w, buf_h);
                                        }
                                    }
                                }
                            }
                        }

                        // Present the compositor-allocated AHB directly.
                        if let Some(ref wm) = backend.window_manager {
                            if let Some(window) = wm.windows.get(&window_id) {
                                if let Some(ref ahb_surface) = window.render.ahb_surface {
                                    if !window.needs_redraw {
                                        done = true;
                                    } else if ahb_surface.frame_in_flight.load(std::sync::atomic::Ordering::Acquire) {
                                        // Previous frame still on screen — wait for vsync.
                                    } else {
                                        let t0 = Instant::now();
                                        let win_size = window.size;
                                        let wake_fd = backend.wake_fd;
                                        let src_x = (geo_offset.x as f64 * scale).round() as i32;
                                        let src_y = (geo_offset.y as f64 * scale).round() as i32;
                                        crate::android::backend::surface_transaction::present_buffer(
                                            &ahb_surface.surface_control,
                                            &ahb_arc,
                                            -1,
                                            buf_w,
                                            buf_h,
                                            win_size.w,
                                            win_size.h,
                                            src_x,
                                            src_y,
                                            &ahb_surface.frame_in_flight,
                                            wake_fd,
                                        );
                                        let frame_us = t0.elapsed().as_micros() as u64;
                                        done = true;
                                        if let Some(wm) = backend.window_manager.as_mut() {
                                            if let Some(w) = wm.windows.get_mut(&window_id) {
                                                w.metrics.last_render_method = "AHB zero-copy";
                                                w.metrics.last_buffer_size = Some((buf_w, buf_h));
                                                w.metrics.last_frame_us = frame_us;
                                                w.needs_redraw = false;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // === AHB / ASurfaceTransaction path (Vulkan blit fallback) ===
                    // Create AHB surface for dmabuf clients that use client-allocated buffers.
                    if !done {
                        let needs_ahb = backend.window_manager.as_ref()
                            .and_then(|wm| wm.windows.get(&window_id))
                            .map(|w| w.render.ahb_surface.is_none() && w.render.native_window.is_some())
                            .unwrap_or(false);

                        if needs_ahb {
                            if let Some(wm) = backend.window_manager.as_mut() {
                                if let Some(window) = wm.windows.get_mut(&window_id) {
                                    if let Some(native_window) = window.render.native_window {
                                        if window.render.egl_surface.is_some() {
                                            tracing::info!("Destroying EGL for AHB takeover window_id={}", window_id);
                                            window.render.egl_surface = None;
                                        }
                                        let sc = crate::android::backend::surface_transaction::SurfaceControlHandle::from_window(
                                            native_window, &format!("wl-{window_id}"));
                                        if let Some(sc) = sc {
                                            match vk.create_ahb_target(buf_w, buf_h) {
                                                Ok(ahb_target) => {
                                                    crate::android::backend::surface_transaction::set_visible(&sc);
                                                    window.render.ahb_surface = Some(crate::android::window_manager::AhbWindowSurface {
                                                        surface_control: sc,
                                                        ahb_target: Some(ahb_target),
                                                        frame_in_flight: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                                                    });
                                                    window.needs_redraw = true;
                                                    tracing::info!("AHB surface created for window_id={} at {}x{}", window_id, buf_w, buf_h);
                                                }
                                                Err(e) => tracing::error!("AHB target creation failed: {e}"),
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        // Resize AHB if buffer dimensions changed
                        let needs_resize = backend.window_manager.as_ref()
                            .and_then(|wm| wm.windows.get(&window_id))
                            .and_then(|w| w.render.ahb_surface.as_ref())
                            .and_then(|ahb| ahb.ahb_target.as_ref())
                            .map(|target| target.width != buf_w || target.height != buf_h)
                            .unwrap_or(false);

                        if needs_resize {
                            if let Some(wm) = backend.window_manager.as_mut() {
                                if let Some(window) = wm.windows.get_mut(&window_id) {
                                    if let Some(ref old) = window.render.ahb_surface {
                                        if let Some(ref target) = old.ahb_target {
                                            vk.destroy_ahb_target(target);
                                        }
                                    }
                                    let sc = window.render.ahb_surface.take().map(|s| s.surface_control);
                                    if let Some(sc) = sc {
                                        match vk.create_ahb_target(buf_w, buf_h) {
                                            Ok(ahb_target) => {
                                                window.render.ahb_surface = Some(crate::android::window_manager::AhbWindowSurface {
                                                    surface_control: sc, ahb_target: Some(ahb_target),
                                                    frame_in_flight: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                                                });
                                                window.needs_redraw = true;
                                                tracing::info!("AHB resized for window_id={} to {}x{}", window_id, buf_w, buf_h);
                                            }
                                            Err(e) => tracing::error!("AHB resize failed: {e}"),
                                        }
                                    }
                                }
                            }
                        }

                        // Blit to AHB + present via ASurfaceTransaction
                        if let Some(ref wm) = backend.window_manager {
                            if let Some(window) = wm.windows.get(&window_id) {
                                if let Some(ref ahb_surface) = window.render.ahb_surface {
                                    if let Some(ref ahb_target) = ahb_surface.ahb_target {
                                    if !window.needs_redraw {
                                        done = true; // no new frame, skip
                                    } else if ahb_surface.frame_in_flight.load(std::sync::atomic::Ordering::Acquire) {
                                        // Previous frame still on screen — don't blit, don't
                                        // send frame callbacks (done stays false → loop skips
                                        // naturally, suppressing client rendering until vsync).
                                    } else {
                                        let t0 = Instant::now();
                                        let sz = dmabuf.size();
                                        let fd = dmabuf.handles().next();
                                        let stride = dmabuf.strides().next().unwrap_or(sz.w as u32 * 4);
                                        let win_size = window.size;
                                        let wake_fd = backend.wake_fd;
                                        if let Some(fd) = fd {
                                            use std::os::unix::io::AsRawFd;
                                            let raw_fd = fd.as_raw_fd();
                                            match vk.get_or_import_dmabuf(raw_fd, sz.w as u32, sz.h as u32, stride, vk_fmt) {
                                                Ok(imported) => {
                                                    match vk.blit_dmabuf_to_ahb(&imported, ahb_target) {
                                                        Ok(fence_fd) => {
                                                            let src_x = (geo_offset.x as f64 * scale).round() as i32;
                                                            let src_y = (geo_offset.y as f64 * scale).round() as i32;
                                                            crate::android::backend::surface_transaction::present_buffer(
                                                                &ahb_surface.surface_control,
                                                                &ahb_target.ahb,
                                                                fence_fd,
                                                                ahb_target.width,
                                                                ahb_target.height,
                                                                win_size.w,
                                                                win_size.h,
                                                                src_x,
                                                                src_y,
                                                                &ahb_surface.frame_in_flight,
                                                                wake_fd,
                                                            );
                                                            let frame_us = t0.elapsed().as_micros() as u64;
                                                            done = true;
                                                            if let Some(wm) = backend.window_manager.as_mut() {
                                                                if let Some(w) = wm.windows.get_mut(&window_id) {
                                                                    w.metrics.last_render_method = "AHB txn";
                                                                    w.metrics.last_buffer_size = Some((sz.w as u32, sz.h as u32));
                                                                    w.metrics.last_frame_us = frame_us;
                                                                    w.needs_redraw = false;
                                                                }
                                                            }
                                                        }
                                                        Err(e) => tracing::warn!("AHB blit failed: {e}"),
                                                    }
                                                }
                                                Err(e) => tracing::warn!("dmabuf import for AHB failed: {e}"),
                                            }
                                        }
                                    }
                                    } // if let Some(ref ahb_target)
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

        // ── shm windows: handled by GLES fallback below ──
        // Previously had a VK shm blit path here to avoid GLES/VK mixing corruption.
        // Now that glFinish() + eglReleaseThread() fixes the corruption (see above),
        // all shm clients (gedit, Firefox, nemo) go through smithay's GLES renderer.
        // VK shm path removed — all shm clients use GLES now.
        // (glFinish + eglReleaseThread prevents GLES/VK corruption.)

        // ── GLES renderer for shm windows ──
        // Render in a scoped block so borrows are released before submit
        {
            let Some(wm) = backend.window_manager.as_mut() else {
                continue;
            };
            let Some(window) = wm.windows.get_mut(&window_id) else {
                continue;
            };
            // Lazy EGL surface creation for shm clients. Only create when:
            // 1. No AHB surface (client doesn't use dmabuf)
            // 2. Window has committed (needs_redraw) — so we know it's wl_shm
            if window.render.egl_surface.is_none() && window.render.ahb_surface.is_none()
                && window.render.native_window.is_some() && window.needs_redraw {
                if let Some(handle) = wm.get_native_handle(window_id) {
                    if let Some(surface) = backend.renderer.as_ref()
                        .and_then(|r| r.create_surface_for_native_window(handle).ok()) {
                        tracing::info!("Lazy-created EGL surface for window_id={}", window_id);
                        if let Some(w) = wm.windows.get_mut(&window_id) {
                            w.render.egl_surface = Some(surface);
                        }
                    }
                }
            }
            let Some(window) = wm.windows.get_mut(&window_id) else {
                continue;
            };
            let Some(egl_surface) = window.render.egl_surface.as_mut() else {
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

            // Only clear+draw when we have content — avoids black flash flicker
            // between buffer releases and new commits.
            if !elements.is_empty() {
                if let Err(e) = frame.clear(Color32F::new(0.0, 0.0, 0.0, 1.0), &[damage]) {
                    tracing::warn!("frame.clear failed for window_id={}: {e:?}", window_id);
                }
                if let Err(e) = draw_render_elements(&mut frame, scale, &elements, &[damage]) {
                    tracing::warn!("draw_render_elements failed for window_id={}: {e:?}", window_id);
                }
            }
            if let Err(e) = frame.finish() {
                tracing::warn!("frame.finish failed for window_id={}: {e:?}", window_id);
            }
            // TODO: intermittent horizontal stripe corruption in Firefox wl_shm path.
            // Likely a race in shm mmap between proot client writes and our texture
            // upload. glFinish doesn't help — corruption is before GL reads.
        }
        // Borrows released — now swap buffers
        {
            let Some(wm) = backend.window_manager.as_ref() else {
                continue;
            };
            let Some(window) = wm.windows.get(&window_id) else {
                continue;
            };
            let Some(egl_surface) = window.render.egl_surface.as_ref() else {
                continue;
            };
            let Some(cr) = backend.renderer.as_ref() else {
                continue;
            };
            let _ = cr.submit_surface(egl_surface);
            // Unbind EGL context after GLES rendering. On Qualcomm, EGL and
            // Vulkan share the GPU via KGSL — leaving EGL bound while the next
            // window does Vulkan AHB blits causes GPU state corruption.
            let _ = cr.renderer.egl_context().unbind();
        }
        // Send frame callbacks AFTER swap + finish — client can now safely
        // reuse its shm buffer.
        send_frames_surface_tree(&wl_surface, time);
        for (popup, _) in PopupManager::popups_for_surface(&wl_surface) {
            send_frames_surface_tree(popup.wl_surface(), time);
        }
        // Count rendered frame for FPS tracking + mark GLES render method.
        if let Some(wm) = backend.window_manager.as_mut()
            && let Some(window) = wm.windows.get_mut(&window_id)
        {
            window.metrics.frame_count += 1;
            if window.metrics.last_render_method != "VK dmabuf" {
                window.metrics.last_render_method = "GLES shm";
            }
        }

    }
}
