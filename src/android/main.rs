use crate::android::{
    app::run_compositor_loop,
    proot::setup,
    utils::application_context::ApplicationContext,
};
use crate::core::config;
use android_activity::{AndroidApp, MainEvent, PollEvent};
use jni::objects::{JClass, JObject, JValue};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[unsafe(no_mangle)]
fn android_main(android_app: AndroidApp) {
    unsafe { std::env::set_var("RUST_BACKTRACE", "full") };

    // Initialize Android logger — Info level to avoid flooding logcat buffer.
    android_logger::init_once(
        android_logger::Config::default().with_max_level(log::LevelFilter::Info),
    );

    // Build the application context (resolves data_dir, native_library_dir, etc.)
    if let Err(e) = ApplicationContext::build(&android_app) {
        log::error!("Failed to build ApplicationContext: {e}");
        return;
    }

    // Point libxkbcommon at the xkb data inside the Arch rootfs.
    // The directory may not exist yet (first run), but it will after setup completes.
    let xkb_path = format!("{}/usr/share/X11/xkb", config::ARCH_FS_ROOT);
    unsafe { std::env::set_var("XKB_CONFIG_ROOT", &xkb_path) };

    // Always fix the xkb symlink (it may be absolute from the rootfs tarball).
    setup::fix_xkb_symlink();

    // Disable bwrap (bubblewrap) — it can't work inside proot.
    setup::disable_bwrap();

    // Check if rootfs is present AND dependencies are installed.
    let needs_setup = !setup::is_setup_complete();

    // Run setup in background so the event loop can start immediately
    // (drawing the first frame dismisses the Android 12+ splash screen).
    let setup_done = Arc::new(AtomicBool::new(!needs_setup));

    if needs_setup {
        let app_clone = android_app.clone();
        setup::set_ui_logger(move |msg| {
            let _ = send_setup_log_jni(&app_clone, msg);
        });

        let done = setup_done.clone();
        let app_clone = android_app.clone();
        std::thread::spawn(move || {
            setup::run_setup();
            setup::clear_ui_logger();
            let _ = hide_setup_overlay(&app_clone);
            done.store(true, Ordering::Release);
            log::info!("Background setup complete");
        });
    }

    // Spawn the compositor on a background thread, independent of NativeActivity lifecycle.
    let compositor_app = android_app.clone();
    let compositor_done = setup_done.clone();
    let _ = std::thread::Builder::new()
        .name("compositor".into())
        .spawn(move || {
            run_compositor_loop(compositor_app, compositor_done);
        });

    // Run a minimal event loop to handle NativeActivity lifecycle.
    // This keeps the native thread alive and processes Android lifecycle events.
    // The compositor runs independently on its own thread.
    let mut overlay_shown = false;
    let mut destroyed = false;
    while !destroyed {
        android_app.poll_events(Some(Duration::from_secs(1)), |event| {
            match event {
                PollEvent::Main(MainEvent::InitWindow { .. }) => {
                    if !overlay_shown && !setup_done.load(Ordering::Acquire) {
                        let _ = show_setup_overlay(&android_app);
                        overlay_shown = true;
                    }
                }
                PollEvent::Main(MainEvent::Destroy) => {
                    destroyed = true;
                }
                _ => {}
            }
        });
    }

    // Exit the process so Android starts fresh on next launch.
    // ndk-context panics if ANativeActivity_onCreate runs twice in the same process.
    std::process::exit(0);
}

// ---- JNI helpers for SetupOverlay ----

fn get_overlay_class<'a>(
    env: &mut jni::JNIEnv<'a>,
    activity: &JObject,
) -> Result<JObject<'a>, jni::errors::Error> {
    let class_loader = env
        .call_method(activity, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])?
        .l()?;
    let class_name = env.new_string("io.github.phiresky.wayland_android.SetupOverlay")?;
    env.call_method(
        &class_loader,
        "loadClass",
        "(Ljava/lang/String;)Ljava/lang/Class;",
        &[JValue::Object(&class_name)],
    )?
    .l()
}

pub(crate) fn show_setup_overlay(android_app: &AndroidApp) -> Result<(), jni::errors::Error> {
    let vm = unsafe { jni::JavaVM::from_raw(android_app.vm_as_ptr() as *mut _) }?;
    let mut env = vm.attach_current_thread()?;
    let activity = unsafe { JObject::from_raw(android_app.activity_as_ptr() as *mut _) };

    let overlay_class = get_overlay_class(&mut env, &activity)?;
    let overlay_jclass: JClass = unsafe { JClass::from_raw(overlay_class.as_raw()) };

    env.call_static_method(
        overlay_jclass,
        "show",
        "(Landroid/app/Activity;)V",
        &[JValue::Object(&activity)],
    )?;

    log::info!("Showing setup overlay");
    Ok(())
}

fn send_setup_log_jni(android_app: &AndroidApp, msg: &str) -> Result<(), jni::errors::Error> {
    let vm = unsafe { jni::JavaVM::from_raw(android_app.vm_as_ptr() as *mut _) }?;
    let mut env = vm.attach_current_thread()?;
    let activity = unsafe { JObject::from_raw(android_app.activity_as_ptr() as *mut _) };

    let overlay_class = get_overlay_class(&mut env, &activity)?;
    let overlay_jclass: JClass = unsafe { JClass::from_raw(overlay_class.as_raw()) };

    let jmsg = env.new_string(msg)?;
    env.call_static_method(
        overlay_jclass,
        "appendLog",
        "(Ljava/lang/String;)V",
        &[JValue::Object(&jmsg)],
    )?;

    Ok(())
}

fn hide_setup_overlay(android_app: &AndroidApp) -> Result<(), jni::errors::Error> {
    let vm = unsafe { jni::JavaVM::from_raw(android_app.vm_as_ptr() as *mut _) }?;
    let mut env = vm.attach_current_thread()?;
    let activity = unsafe { JObject::from_raw(android_app.activity_as_ptr() as *mut _) };

    let overlay_class = get_overlay_class(&mut env, &activity)?;
    let overlay_jclass: JClass = unsafe { JClass::from_raw(overlay_class.as_raw()) };

    env.call_static_method(
        overlay_jclass,
        "hide",
        "(Landroid/app/Activity;)V",
        &[JValue::Object(&activity)],
    )?;

    log::info!("Setup overlay hidden");
    Ok(())
}
