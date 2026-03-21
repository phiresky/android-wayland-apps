use crate::android::{
    backend::{
        compositor_tick, dispatch_wayland, drain_wake, init_egl_headless, WaylandBackend,
    },
    compositor::{Compositor, State},
    proot::launch::launch,
    utils::jni_context,
    window_manager::{self, WindowManager},
};
use smithay::output::{Mode, Output, PhysicalProperties, Scale, Subpixel};
use smithay::utils::Transform;
use std::os::unix::io::{AsFd, AsRawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Run the compositor event loop on the current thread.
/// Called from a background thread spawned in nativeInit.
/// Independent of Activity lifecycle — keeps running even if the Activity is destroyed.
pub fn run_compositor_loop(setup_done: Arc<AtomicBool>) {
    // Create eventfd for waking the compositor from JNI/Wayland commits.
    let wake_fd = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK) };
    if wake_fd < 0 {
        tracing::error!("Failed to create eventfd");
        return;
    }

    // Init headless EGL (no window surface needed).
    let renderer = match init_egl_headless() {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("Failed to init headless EGL: {e}");
            return;
        }
    };

    // Query supported dmabuf formats from the EGL renderer.
    // Android EGL lacks EGL_EXT_image_dma_buf_import, but we advertise common formats
    // anyway so Vulkan clients can create swapchains. Import is handled at render time.
    use smithay::backend::renderer::ImportDma;
    use smithay::backend::allocator::format::FormatSet;
    let mut dmabuf_formats = renderer.renderer.dmabuf_formats();
    if dmabuf_formats.iter().next().is_none() {
        tracing::warn!("EGL has no dmabuf formats — advertising common formats for Vulkan WSI");
        dmabuf_formats = FormatSet::from_formats_hardcoded();
    }

    // Build compositor (always advertises dmabuf global for Vulkan client support).
    let mut compositor = match Compositor::build(dmabuf_formats) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("Failed to build compositor: {e}");
            return;
        }
    };
    compositor.state.wake_fd = Some(wake_fd);

    // Query display info via JNI.
    let (scale_factor, display_w, display_h) = get_display_info().unwrap_or((2.0, 2160, 1584));
    tracing::info!("Display: {display_w}x{display_h}, density: {scale_factor}");

    // Create wl_output with the device's physical display resolution.
    let output = Output::new(
        "Android Wayland Launcher".into(),
        PhysicalProperties {
            size: (display_w, display_h).into(),
            subpixel: Subpixel::HorizontalRgb,
            make: "Android".into(),
            model: "Wayland Launcher".into(),
            serial_number: String::new(),
        },
    );
    let dh = compositor.display.handle();
    let _global = output.create_global::<State>(&dh);
    output.change_current_state(
        Some(Mode {
            size: (display_w, display_h).into(),
            refresh: 60000,
        }),
        Some(Transform::Normal),
        Some(Scale::Fractional(scale_factor)),
        Some((0, 0).into()),
    );
    compositor.output = Some(output);

    // Set up window manager.
    window_manager::set_wake_fd(wake_fd);
    let window_manager = WindowManager::new();

    // Init Vulkan renderer (proprietary Qualcomm driver, for zero-copy dmabuf compositing).
    // Per-client override: launch with WAYLAND_ANDROID_RENDER_MODE=gles to force GLES path.
    let vk_renderer = match crate::android::backend::vulkan_renderer::VulkanRenderer::new() {
        Ok(vk) => {
            tracing::info!("Vulkan renderer initialized");
            Some(vk)
        }
        Err(e) => {
            tracing::warn!("Vulkan renderer init failed: {e}");
            None
        }
    };

    // Init AHB allocator for server-side buffer allocation (zero-copy path).
    let mut ahb_allocator = crate::android::backend::ahb_allocator::AhbAllocator::new();
    let mut ahb_tracker = crate::android::backend::ahb_allocator::AhbBufferTracker::new();

    // Integration test: allocate AHB → export dmabuf → track → verify inode lookup
    {
        use smithay::backend::allocator::{Allocator, Fourcc, Modifier};
        use smithay::backend::allocator::dmabuf::AsDmabuf;

        match ahb_allocator.create_buffer(128, 128, Fourcc::Abgr8888, &[Modifier::Invalid]) {
            Ok(buf) => {
                tracing::info!("[ahb-test] Allocated 128x128 AHB, stride={}B", buf.stride);
                match ahb_tracker.track(buf) {
                    Ok(dmabuf) => {
                        // Verify the tracker can find this buffer by its dmabuf inode
                        if ahb_tracker.lookup(&dmabuf).is_some() {
                            tracing::info!("[ahb-test] PASS: tracker found buffer by inode");
                        } else {
                            tracing::error!("[ahb-test] FAIL: tracker lookup returned None");
                        }
                        // Clean up test buffer
                        ahb_tracker.untrack(&dmabuf);
                    }
                    Err(e) => tracing::error!("[ahb-test] FAIL: dmabuf export failed: {e}"),
                }
            }
            Err(e) => tracing::error!("[ahb-test] FAIL: AHB allocation failed: {e}"),
        }
    }

    let ahb_allocator = Some(ahb_allocator);

    // Start GBM allocator server for proot clients.
    // Clients connect to {ARCH_FS_ROOT}/tmp/gbm-alloc-0 and get dmabuf fds
    // backed by AHardwareBuffers. The server's tracker is shared with the
    // compositor so it can recognize these buffers at commit time.
    let gbm_state = crate::android::backend::gbm_server::start_server(crate::core::config::ARCH_FS_ROOT);
    tracing::info!("GBM allocator server started");

    let mut backend = WaylandBackend {
        compositor,
        renderer: Some(renderer),
        vk_renderer,
        window_manager: Some(window_manager),
        wake_fd,
        scale_factor,
        ahb_allocator,
        ahb_tracker,
    };

    // Wait for rootfs setup to complete, then init keyboard and launch proot.
    while !setup_done.load(Ordering::Acquire) {
        // Still dispatch Wayland protocol while waiting (clients might connect early).
        dispatch_wayland(&mut backend);
        std::thread::sleep(Duration::from_millis(100));
    }
    backend.compositor.init_keyboard();
    launch();
    tracing::info!("Compositor loop started");

    // Main poll loop.
    let listener_fd = backend.compositor.listener.as_raw_fd();
    let display_fd = backend.compositor.display.as_fd().as_raw_fd();

    let mut fds = [
        libc::pollfd { fd: wake_fd, events: libc::POLLIN, revents: 0 },
        libc::pollfd { fd: listener_fd, events: libc::POLLIN, revents: 0 },
        libc::pollfd { fd: display_fd, events: libc::POLLIN, revents: 0 },
    ];

    loop {
        for fd in &mut fds {
            fd.revents = 0;
        }
        // Timeout of 1000ms so status overlay updates even with no activity.
        let ret = unsafe { libc::poll(fds.as_mut_ptr(), 3, 1000) };

        if ret < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() != std::io::ErrorKind::Interrupted {
                tracing::error!("Compositor poll failed: {err}");
                break;
            }
            continue;
        }

        // Drain eventfd.
        if fds[0].revents & libc::POLLIN != 0 {
            drain_wake(wake_fd);
        }

        compositor_tick(&mut backend);
    }
}

/// Query display density and physical resolution from Android via JNI.
/// Uses getRealMetrics() to get the full display size (not the app window size).
fn get_display_info() -> Result<(f64, i32, i32), jni::errors::Error> {
    jni_context::with_jni(|env, activity| {
        let resources = env
            .call_method(activity, "getResources", "()Landroid/content/res/Resources;", &[])?
            .l()?;
        let metrics = env
            .call_method(&resources, "getDisplayMetrics", "()Landroid/util/DisplayMetrics;", &[])?
            .l()?;
        let density = env.get_field(&metrics, "density", "F")?.f()? as f64;

        // getRealMetrics gives full display size (not windowed app size in DeX)
        let wm = env
            .call_method(activity, "getSystemService", "(Ljava/lang/String;)Ljava/lang/Object;",
                &[jni::objects::JValue::Object(&jni::objects::JObject::from(env.new_string("window")?))])?
            .l()?;
        let display = env
            .call_method(&wm, "getDefaultDisplay", "()Landroid/view/Display;", &[])?
            .l()?;
        let real_metrics = env.new_object("android/util/DisplayMetrics", "()V", &[])?;
        env.call_method(&display, "getRealMetrics", "(Landroid/util/DisplayMetrics;)V",
            &[jni::objects::JValue::Object(&real_metrics)])?;
        let width = env.get_field(&real_metrics, "widthPixels", "I")?.i()?;
        let height = env.get_field(&real_metrics, "heightPixels", "I")?.i()?;
        Ok((density, width, height))
    })
}
