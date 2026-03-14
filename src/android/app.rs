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
        log::error!("Failed to create eventfd");
        return;
    }

    // Init headless EGL (no window surface needed).
    let renderer = match init_egl_headless() {
        Ok(r) => r,
        Err(e) => {
            log::error!("Failed to init headless EGL: {e}");
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
        log::warn!("EGL has no dmabuf formats — advertising common formats for Vulkan WSI");
        dmabuf_formats = FormatSet::from_formats_hardcoded();
    }

    // Build compositor (always advertises dmabuf global for Vulkan client support).
    let mut compositor = match Compositor::build(dmabuf_formats) {
        Ok(c) => c,
        Err(e) => {
            log::error!("Failed to build compositor: {e}");
            return;
        }
    };
    compositor.state.wake_fd = Some(wake_fd);

    // Query display density via JNI.
    let scale_factor = get_display_density().unwrap_or(2.0);
    log::info!("Display density: {scale_factor}");

    // Create wl_output with default size (updated when activities report dimensions).
    let output = Output::new(
        "Android Wayland Launcher".into(),
        PhysicalProperties {
            size: (1920, 1080).into(),
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
            size: (1920, 1080).into(),
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

    let mut backend = WaylandBackend {
        compositor,
        renderer: Some(renderer),
        window_manager: Some(window_manager),
        wake_fd,
        scale_factor,
    };

    // Wait for rootfs setup to complete, then init keyboard and launch proot.
    while !setup_done.load(Ordering::Acquire) {
        // Still dispatch Wayland protocol while waiting (clients might connect early).
        dispatch_wayland(&mut backend);
        std::thread::sleep(Duration::from_millis(100));
    }
    backend.compositor.init_keyboard();
    launch();
    log::info!("Compositor loop started");

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
                log::error!("Compositor poll failed: {err}");
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

/// Query the display density (scale factor) from Android DisplayMetrics via JNI.
fn get_display_density() -> Result<f64, jni::errors::Error> {
    jni_context::with_jni(|env, activity| {
        // activity.getResources().getDisplayMetrics().density
        let resources = env
            .call_method(
                activity,
                "getResources",
                "()Landroid/content/res/Resources;",
                &[],
            )?
            .l()?;
        let metrics = env
            .call_method(
                &resources,
                "getDisplayMetrics",
                "()Landroid/util/DisplayMetrics;",
                &[],
            )?
            .l()?;
        let density = env.get_field(&metrics, "density", "F")?.f()?;
        Ok(density as f64)
    })
}
