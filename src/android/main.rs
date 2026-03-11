use crate::android::{
    app::App,
    proot::setup,
    utils::application_context::ApplicationContext,
};
use crate::core::config;
use jni::objects::{JClass, JObject, JValue};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use winit::{
    event_loop::{ControlFlow, EventLoop},
    platform::android::{activity::AndroidApp, EventLoopBuilderExtAndroid},
};

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

    let event_loop = match EventLoop::builder()
        .with_android_app(android_app.clone())
        .build()
    {
        Ok(el) => el,
        Err(e) => {
            log::error!("Failed to create event loop: {e}");
            return;
        }
    };

    event_loop.set_control_flow(ControlFlow::Wait);

    let app = match App::build(android_app, setup_done) {
        Ok(a) => a,
        Err(e) => {
            log::error!("Failed to build App: {e}");
            return;
        }
    };
    // winit 0.31 requires 'static for run_app; leak to satisfy that.
    let app: &'static mut App = Box::leak(Box::new(app));
    if let Err(e) = event_loop.run_app(app) {
        log::error!("Failed to run app: {e}");
    }
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
