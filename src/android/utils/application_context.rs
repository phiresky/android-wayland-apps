use jni::{
    objects::{JObject, JString},
    JNIEnv,
};
use std::path::PathBuf;
use std::sync::OnceLock;

#[derive(Debug)]
pub struct ApplicationContext {
    pub cache_dir: PathBuf,
    pub data_dir: PathBuf,
    pub native_library_dir: PathBuf,
}

impl ApplicationContext {
    pub fn build(env: &mut JNIEnv, activity: &JObject) -> Result<(), Box<dyn std::error::Error>> {
        let ctx = ApplicationContext {
            cache_dir: Self::get_path(env, activity, "getCacheDir")?,
            data_dir: Self::get_path(env, activity, "getFilesDir")?,
            native_library_dir: Self::get_native_library_dir(env, activity)?,
        };
        tracing::info!("ApplicationContext initialized: {:?}", ctx);
        let _ = APPLICATION_CONTEXT.set(ctx);
        Ok(())
    }

    fn get_path(env: &mut JNIEnv, activity: &JObject, method: &str) -> Result<PathBuf, jni::errors::Error> {
        let path_obj = env
            .call_method(activity, method, "()Ljava/io/File;", &[])?
            .l()?;
        let path_str = env
            .call_method(path_obj, "getAbsolutePath", "()Ljava/lang/String;", &[])?
            .l()?;
        let path: String = env
            .get_string(&JString::from(path_str))?
            .into();
        Ok(PathBuf::from(path))
    }

    fn get_native_library_dir(env: &mut JNIEnv, activity: &JObject) -> Result<PathBuf, jni::errors::Error> {
        let app_info = env
            .call_method(
                activity,
                "getApplicationInfo",
                "()Landroid/content/pm/ApplicationInfo;",
                &[],
            )?
            .l()?;
        let native_library_dir = env
            .get_field(app_info, "nativeLibraryDir", "Ljava/lang/String;")?
            .l()?;
        let path: String = env
            .get_string(&JString::from(native_library_dir))?
            .into();
        Ok(PathBuf::from(path))
    }
}

static APPLICATION_CONTEXT: OnceLock<ApplicationContext> = OnceLock::new();

/// Get the application context. Panics if called before `ApplicationContext::build()`.
/// This is a programming error — build() runs in nativeInit before any other code.
pub fn get_application_context() -> &'static ApplicationContext {
    APPLICATION_CONTEXT.get().unwrap_or_else(|| {
        panic!("ApplicationContext not initialized — build() must be called first")
    })
}
