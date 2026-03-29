use crate::android::{
    app::run_compositor_loop,
    proot::setup,
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
    setup::setup_firefox_config();

    // Configure Electron/Chromium apps (no-sandbox, Wayland). Runs every startup
    // because apps may be installed after initial setup.
    setup::setup_electron_config();

    // Disable bwrap/flatpak-spawn — sandboxing can't work inside proot.
    setup::disable_bwrap();
    setup::disable_flatpak_spawn();

    // Ensure D-Bus and portal configs are up to date (may need regeneration
    // after code changes, even though rootfs already exists).
    setup::setup_flatpak_dbus();
    setup::setup_portal();

    // Build the libhybris Vulkan ICD if not already installed.
    // Runs in foreground since first build takes a few minutes.
    setup::setup_hybris_vulkan();

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
