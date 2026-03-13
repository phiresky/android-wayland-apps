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
    if let Err(e) = open_launcher_activity() {
        log::error!("Failed to open launcher activity: {e}");
    }
}

fn open_launcher_activity() -> Result<(), jni::errors::Error> {
    jni_context::with_jni(|env, activity| {
        let class_loader = env
            .call_method(activity, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])?
            .l()?;
        let class_name =
            env.new_string("io.github.phiresky.wayland_android.LauncherActivity")?;
        let launcher_class = env
            .call_method(
                &class_loader,
                "loadClass",
                "(Ljava/lang/String;)Ljava/lang/Class;",
                &[JValue::Object(&class_name)],
            )?
            .l()?;

        let intent_class = env.find_class("android/content/Intent")?;
        let intent = env.new_object(
            &intent_class,
            "(Landroid/content/Context;Ljava/lang/Class;)V",
            &[JValue::Object(activity), JValue::Object(&launcher_class)],
        )?;

        env.call_method(
            activity,
            "startActivity",
            "(Landroid/content/Intent;)V",
            &[JValue::Object(&intent)],
        )?;

        log::info!("Opened LauncherActivity");
        Ok(())
    })
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
            log::error!("Failed to get command string: {e}");
            return;
        }
    };

    thread::spawn(move || {
        log::info!("Launching app: {}", command);
        let output = ArchProcess {
            command,
            user: Some(config::DEFAULT_USERNAME.to_string()),
            log: Some(Arc::new(|it| log::info!("{}", it))),
        }
        .run();
        log::info!("App exited: {:?}", output.status);
    });
}
