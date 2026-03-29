use super::process::ArchProcess;
use crate::android::utils::jni_context;
use crate::core::config;
use jni::objects::{JObject, JString, JValue};
use jni::JNIEnv;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

/// Spawn a background thread that runs a command in proot, logs its output
/// with `[prefix]`, and warns when it exits. An optional `delay` causes the
/// thread to sleep before launching the process.
fn start_background_process(
    command: &str,
    prefix: &str,
    kill_on_exit: bool,
    delay: Option<Duration>,
) {
    let command = command.to_string();
    let prefix = prefix.to_string();
    thread::spawn(move || {
        if let Some(d) = delay {
            thread::sleep(d);
        }
        tracing::info!("Starting {prefix} in proot...");
        let log_prefix = prefix.clone();
        let output = ArchProcess {
            command,
            user: Some(config::USERNAME.to_string()),
            log: Some(Arc::new(move |line| tracing::info!("[{log_prefix}] {}", line))),
            kill_on_exit,
        }
        .run();
        tracing::warn!("{prefix} exited: {:?}", output.status);
    });
}

/// Open the native Android launcher Activity.
/// Called once after setup completes and the compositor is ready.
pub fn launch() {
    if crate::core::config::pipewire_enabled() {
        start_pipewire();
    }
    start_portal();
    if let Err(e) = open_launcher_activity() {
        tracing::error!("Failed to open launcher activity: {e}");
    }
}

/// Start PipeWire and WirePlumber daemons inside proot.
/// Each runs as a foreground process in its own long-lived proot instance.
/// The /tmp/pipewire-0 socket is created by PipeWire for clients to connect.
fn start_pipewire() {
    // Clean stale PipeWire sockets from previous runs
    let pw_socket = format!("{}/tmp/pipewire-0", config::ARCH_FS_ROOT);
    let _ = std::fs::remove_file(&pw_socket);
    let _ = std::fs::remove_file(format!("{}-manager", pw_socket));

    // PipeWire daemon — runs as foreground (blocks the thread)
    // kill_on_exit=false because PipeWire daemonizes (forks)
    start_background_process(
        "PIPEWIRE_DEBUG=4 pipewire & sleep infinity",
        "pipewire",
        false,
        None,
    );

    // WirePlumber session manager — start after a short delay
    start_background_process(
        "wireplumber",
        "wireplumber",
        false,
        Some(Duration::from_secs(2)),
    );
}

/// Start D-Bus session bus and our standalone portal daemon inside proot.
/// Replaces xdg-desktop-portal entirely (it needs /proc access proot can't provide).
fn start_portal() {
    // Source start-dbus to ensure session bus is running, then start our portal.
    start_background_process(
        ". /usr/local/bin/start-dbus; /usr/local/libexec/xdg-desktop-portal-android",
        "portal",
        false,
        None,
    );
}

fn open_launcher_activity() -> Result<(), jni::errors::Error> {
    jni_context::with_jni(|env, activity| {
        let launcher_class = jni_context::load_class(
            env, activity, "io.github.phiresky.wayland_android.LauncherActivity",
        )?;

        let intent_class = env.find_class("android/content/Intent")?;
        let intent = env.new_object(
            &intent_class,
            "(Landroid/content/Context;Ljava/lang/Class;)V",
            &[JValue::Object(activity), JValue::Object(&launcher_class.into())],
        )?;

        // Pass launcher config as intent extras
        let ignore = to_java_string_array(env, config::LAUNCHER_IGNORE)?;
        env.call_method(
            &intent, "putExtra",
            "(Ljava/lang/String;[Ljava/lang/String;)Landroid/content/Intent;",
            &[JValue::Object(&env.new_string("ignore")?.into()), JValue::Object(&ignore)],
        )?;

        let extra_names: Vec<&str> = config::LAUNCHER_EXTRA.iter().map(|(n, _, _)| *n).collect();
        let extra_execs: Vec<&str> = config::LAUNCHER_EXTRA.iter().map(|(_, e, _)| *e).collect();
        let extra_icons: Vec<&str> = config::LAUNCHER_EXTRA.iter().map(|(_, _, i)| *i).collect();
        let names_arr = to_java_string_array(env, &extra_names)?;
        let execs_arr = to_java_string_array(env, &extra_execs)?;
        let icons_arr = to_java_string_array(env, &extra_icons)?;
        env.call_method(
            &intent, "putExtra",
            "(Ljava/lang/String;[Ljava/lang/String;)Landroid/content/Intent;",
            &[JValue::Object(&env.new_string("extra_names")?.into()), JValue::Object(&names_arr)],
        )?;
        env.call_method(
            &intent, "putExtra",
            "(Ljava/lang/String;[Ljava/lang/String;)Landroid/content/Intent;",
            &[JValue::Object(&env.new_string("extra_execs")?.into()), JValue::Object(&execs_arr)],
        )?;
        env.call_method(
            &intent, "putExtra",
            "(Ljava/lang/String;[Ljava/lang/String;)Landroid/content/Intent;",
            &[JValue::Object(&env.new_string("extra_icons")?.into()), JValue::Object(&icons_arr)],
        )?;

        env.call_method(
            activity,
            "startActivity",
            "(Landroid/content/Intent;)V",
            &[JValue::Object(&intent)],
        )?;

        tracing::info!("Opened LauncherActivity");
        Ok(())
    })
}

fn to_java_string_array<'a>(env: &mut JNIEnv<'a>, items: &[&str]) -> Result<JObject<'a>, jni::errors::Error> {
    let string_class = env.find_class("java/lang/String")?;
    let array = env.new_object_array(items.len() as i32, &string_class, JObject::null())?;
    for (i, item) in items.iter().enumerate() {
        let s = env.new_string(item)?;
        env.set_object_array_element(&array, i as i32, s)?;
    }
    Ok(array.into())
}

/// JNI export: called from LauncherActivity when the user taps an app.
#[unsafe(no_mangle)]
extern "system" fn Java_io_github_phiresky_wayland_1android_LauncherActivity_nativeLaunchApp(
    mut env: JNIEnv,
    _class: JObject,
    command: JString,
) {
    let command: String = match env.get_string(&command) {
        Ok(s) => s.into(),
        Err(e) => {
            tracing::error!("Failed to get command string: {e}");
            return;
        }
    };

    start_background_process(&command, "app", true, None);
}
