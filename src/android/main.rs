// ==========================================================================
// JNI Entry Point Index
// ==========================================================================
//
// All `#[unsafe(no_mangle)]` JNI exports in this crate, grouped by Java class.
// Each entry lists the Rust function name and its source file.
//
// ## JNI_OnLoad
//   JNI_OnLoad                          — src/android/main.rs
//       Called by System.loadLibrary; caches the JavaVM pointer.
//
// ## MainActivity (src/android/main.rs, src/android/jni_exports.rs)
//   nativeInit                           — src/android/main.rs
//       Compositor init, rootfs setup, spawns compositor thread.
//   nativeSetPipewireEnabled             — src/android/jni_exports.rs
//       Restores saved PipeWire preference on startup.
//
// ## WaylandWindowActivity (src/android/jni_exports.rs)
//   nativeSurfaceCreated                 — Acquires ANativeWindow from Surface.
//   nativeSurfaceChanged                 — Handles surface resize.
//   nativeSurfaceDestroyed               — Releases native window.
//   nativeWindowClosed                   — Activity destroyed / finishing.
//   nativeRequestClose                   — User requested close (back / DeX X).
//   nativeOnTouchEvent                   — Touch input forwarding.
//   nativeOnKeyEvent                     — Key input forwarding.
//   nativeOnImeText                      — IME compose/commit/delete/recompose.
//   nativeRightClick                     — Right-click (long press / mouse).
//
// ## DebugActivity (src/android/jni_exports.rs)
//   nativeSetVulkanRendering             — Toggle Vulkan vs GLES render mode.
//   nativeGetVulkanRendering             — Query current render mode.
//   nativeSetZeroCopyEnabled             — Toggle zero-copy compositing.
//   nativeGetZeroCopyEnabled             — Query zero-copy state.
//   nativeSetPipewireEnabled             — Toggle PipeWire from debug UI.
//   nativeGetPipewireEnabled             — Query PipeWire state.
//   nativeGetDebugLog                    — Retrieve debug log buffer.
//
// ## FileChooserActivity (src/android/portal.rs)
//   nativeFileChooserResult              — XDG Desktop Portal file chooser callback.
//
// ## LauncherActivity (src/android/proot/launch.rs)
//   nativeLaunchApp                      — Launch a Linux app from the launcher UI.
//
// ==========================================================================

use crate::android::{
    app::run_compositor_loop,
    proot::{app_compat, services, setup},
    utils::{application_context::ApplicationContext, jni_context},
};
use crate::core::config;
use jni::objects::{JObject, JValue};
use jni::sys::jint;
use jni::JNIEnv;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Called by the JNI runtime when `System.loadLibrary` loads this .so.
#[unsafe(no_mangle)]
extern "system" fn JNI_OnLoad(
    vm: *mut jni::sys::JavaVM,
    _reserved: *mut std::ffi::c_void,
) -> jint {
    let vm = match unsafe { jni::JavaVM::from_raw(vm) } {
        Ok(vm) => vm,
        Err(_) => return -1,
    };
    jni_context::set_vm(vm);
    jni::sys::JNI_VERSION_1_6
}

/// Guard against double-init (process survives Activity restart via foreground service).
static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Called from MainActivity.onCreate() to initialize the compositor.
/// Returns true if first-run setup is needed (caller should show the setup overlay).
#[unsafe(no_mangle)]
extern "system" fn Java_io_github_phiresky_wayland_1android_MainActivity_nativeInit(
    mut env: JNIEnv,
    _class: JObject,
    activity: JObject,
) -> bool {
    if INITIALIZED.swap(true, Ordering::SeqCst) {
        tracing::info!("nativeInit called again — compositor already running");
        return false;
    }

    unsafe { std::env::set_var("RUST_BACKTRACE", "full") };

    // Initialize tracing subscriber for Android logcat output.
    crate::android::utils::android_tracing::init();

    // Smoke-test minigbm (AHardwareBuffer-backed GBM API).
    #[cfg(feature = "zero-copy")]
    match minigbm::smoke_test() {
        Ok(msg) => tracing::info!("[minigbm] {msg}"),
        Err(e) => tracing::error!("[minigbm] smoke test FAILED: {e}"),
    }

    // Store global JNI context (VM + Activity).
    jni_context::init(&mut env, &activity);

    // Build the application context (resolves data_dir, native_library_dir, etc.)
    if let Err(e) = ApplicationContext::build(&mut env, &activity) {
        tracing::error!("Failed to build ApplicationContext: {e}");
        return false;
    }

    // Point libxkbcommon at the xkb data inside the Arch rootfs.
    // The directory may not exist yet (first run), but it will after setup completes.
    let xkb_path = format!("{}/usr/share/X11/xkb", config::ARCH_FS_ROOT);
    unsafe { std::env::set_var("XKB_CONFIG_ROOT", &xkb_path) };

    // Always fix the xkb symlink (it may be absolute from the rootfs tarball).
    setup::fix_xkb_symlink();

    // Configure Firefox for proot (sandbox disable). Runs every startup because
    // Firefox may be installed after initial setup.
    app_compat::setup_firefox_config();

    // Configure Electron/Chromium apps (no-sandbox, Wayland). Runs every startup
    // because apps may be installed after initial setup.
    app_compat::setup_electron_config();

    // Disable bwrap/flatpak-spawn — sandboxing can't work inside proot.
    app_compat::disable_bwrap();
    app_compat::disable_flatpak_spawn();

    // Ensure D-Bus and portal configs are up to date (may need regeneration
    // after code changes, even though rootfs already exists).
    services::setup_flatpak_dbus();
    services::setup_portal();

    // Build the libhybris Vulkan ICD if not already installed.
    // Runs in foreground since first build takes a few minutes.
    services::setup_hybris_vulkan();

    // Check if rootfs is present AND dependencies are installed.
    let needs_setup = !setup::is_setup_complete();

    // Run setup in background so the Activity can start immediately.
    let setup_done = Arc::new(AtomicBool::new(!needs_setup));

    if needs_setup {
        setup::set_ui_logger(|msg| {
            let _ = send_setup_log_jni(msg);
        });

        let done = setup_done.clone();
        std::thread::spawn(move || {
            setup::run_setup();
            setup::clear_ui_logger();
            let _ = hide_setup_overlay();
            done.store(true, Ordering::Release);
            tracing::info!("Background setup complete");
        });
    }

    if crate::core::config::pipewire_enabled() {
        crate::android::camera::start();
        crate::android::audio::start();
    }

    // Spawn the compositor on a background thread, independent of Activity lifecycle.
    let compositor_done = setup_done;
    let _ = std::thread::Builder::new()
        .name("compositor".into())
        .spawn(move || {
            run_compositor_loop(compositor_done);
        });

    needs_setup
}

// ---- JNI helpers for SetupOverlay ----

const SETUP_OVERLAY_CLASS: &str = "io.github.phiresky.wayland_android.SetupOverlay";

fn send_setup_log_jni(msg: &str) -> Result<(), jni::errors::Error> {
    jni_context::with_jni(|env, activity| {
        let class = jni_context::load_class(env, activity, SETUP_OVERLAY_CLASS)?;
        let jmsg = env.new_string(msg)?;
        env.call_static_method(
            class,
            "appendLog",
            "(Ljava/lang/String;)V",
            &[JValue::Object(&jmsg)],
        )?;
        Ok(())
    })
}

fn hide_setup_overlay() -> Result<(), jni::errors::Error> {
    jni_context::with_jni(|env, activity| {
        let class = jni_context::load_class(env, activity, SETUP_OVERLAY_CLASS)?;
        env.call_static_method(
            class,
            "hide",
            "(Landroid/app/Activity;)V",
            &[JValue::Object(activity)],
        )?;
        tracing::info!("Setup overlay hidden");
        Ok(())
    })
}
