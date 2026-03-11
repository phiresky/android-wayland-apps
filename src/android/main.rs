use crate::android::{
    app::App,
    utils::application_context::ApplicationContext,
};
use crate::core::config;
use std::fs;
use std::path::Path;
use winit::{
    event_loop::{ControlFlow, EventLoop},
    platform::android::{activity::AndroidApp, EventLoopBuilderExtAndroid},
};

#[unsafe(no_mangle)]
fn android_main(android_app: AndroidApp) {
    unsafe { std::env::set_var("RUST_BACKTRACE", "full") };

    // Point libxkbcommon at the xkb data inside the Arch rootfs.
    // Also fix the symlink if it's absolute (won't resolve outside proot).
    let xkb_path = format!("{}/usr/share/X11/xkb", config::ARCH_FS_ROOT);
    fix_xkb_symlink(Path::new(&xkb_path));
    unsafe { std::env::set_var("XKB_CONFIG_ROOT", &xkb_path) };

    // Initialize Android logger with trace-level output.
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Trace),
    );

    // Build the application context (resolves data_dir, native_library_dir, etc.)
    ApplicationContext::build(&android_app);

    let event_loop = EventLoop::builder()
        .with_android_app(android_app.clone())
        .build()
        .expect("Failed to create event loop");

    event_loop.set_control_flow(ControlFlow::Wait);

    let app = App::build(android_app);
    // winit 0.31 requires 'static for run_app; leak to satisfy that.
    let app: &'static mut App = Box::leak(Box::new(app));
    event_loop.run_app(app).expect("Failed to run app");
}

/// Ported from localdesktop `src/android/proot/setup.rs:fix_xkb_symlink`.
///
/// In Arch, `/usr/share/X11/xkb` is often an absolute symlink to `/usr/share/xkeyboard-config-2`.
/// Since libxkbcommon runs natively on Android (not inside proot), absolute symlinks don't resolve
/// — they'd look for `/usr/share/xkeyboard-config-2` on the Android root, not inside ARCH_FS_ROOT.
/// Fix by converting to a relative symlink.
fn fix_xkb_symlink(xkb_path: &Path) {
    let Ok(meta) = fs::symlink_metadata(xkb_path) else { return };
    if !meta.file_type().is_symlink() {
        return;
    }
    let Ok(target) = fs::read_link(xkb_path) else { return };
    if !target.is_absolute() {
        return;
    }

    // The symlink target is absolute inside the chroot (e.g. /usr/share/xkeyboard-config-2).
    // We need a relative symlink from xkb_path's parent to ARCH_FS_ROOT + target.
    // E.g. {ROOT}/usr/share/X11/xkb -> ../xkeyboard-config-2
    let stripped = target.strip_prefix("/").unwrap_or(&target);
    let real_target = Path::new(config::ARCH_FS_ROOT).join(stripped);
    let parent = xkb_path.parent().unwrap();

    // Build relative path: count parent components to strip, then append target remainder
    let parent_components: Vec<_> = parent.components().collect();
    let target_components: Vec<_> = real_target.components().collect();
    let common = parent_components.iter().zip(target_components.iter())
        .take_while(|(a, b)| a == b).count();
    let ups = parent_components.len() - common;
    let mut rel = std::path::PathBuf::new();
    for _ in 0..ups {
        rel.push("..");
    }
    for comp in &target_components[common..] {
        rel.push(comp);
    }

    log::info!("Fixing xkb symlink: {} -> {}", xkb_path.display(), rel.display());
    let _ = fs::remove_file(xkb_path);
    if let Err(e) = std::os::unix::fs::symlink(&rel, xkb_path) {
        log::error!("Failed to create xkb symlink: {}", e);
    }
}
