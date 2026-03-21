use super::process::ArchProcess;
use crate::android::utils::jni_context;
use crate::core::config;
use jni::objects::{JObject, JString, JValue};
use jni::JNIEnv;
use std::sync::Arc;
use std::thread;

/// Open the native Android launcher Activity.
/// Called once after setup completes and the compositor is ready.
pub fn launch() {
    start_pipewire();
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
    thread::spawn(|| {
        tracing::info!("Starting PipeWire daemon in proot...");
        let output = ArchProcess {
            command: "PIPEWIRE_DEBUG=4 pipewire & sleep infinity".into(),
            user: Some(config::USERNAME.to_string()),
            log: Some(Arc::new(|line| tracing::info!("[pipewire] {}", line))),
            kill_on_exit: false, // PipeWire daemonizes (forks); don't kill the child
        }
        .run();
        tracing::warn!("PipeWire exited: {:?}", output.status);
    });

    // WirePlumber session manager — start after a short delay
    thread::spawn(|| {
        thread::sleep(std::time::Duration::from_secs(2));
        tracing::info!("Starting WirePlumber in proot...");
        let output = ArchProcess {
            command: "wireplumber".into(),
            user: Some(config::USERNAME.to_string()),
            log: Some(Arc::new(|line| tracing::info!("[wireplumber] {}", line))),
            kill_on_exit: false,
        }
        .run();
        tracing::warn!("WirePlumber exited: {:?}", output.status);
    });
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

    thread::spawn(move || {
        tracing::info!("Launching app: {}", command);
        let output = ArchProcess {
            command,
            user: Some(config::USERNAME.to_string()),
            log: Some(Arc::new(|it| tracing::info!("{}", it))),
            kill_on_exit: true,
        }
        .run();
        tracing::info!("App exited: {:?}", output.status);
    });
}
