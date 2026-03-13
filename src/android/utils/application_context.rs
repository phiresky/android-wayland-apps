use jni::{
    objects::{JObject, JString},
    JNIEnv,
};
use std::path::PathBuf;
use std::sync::RwLock;

#[derive(Debug, Clone)]
pub struct ApplicationContext {
    pub cache_dir: PathBuf,
    pub data_dir: PathBuf,
    pub native_library_dir: PathBuf,
}

impl ApplicationContext {
    pub fn build(env: &mut JNIEnv, activity: &JObject) -> Result<(), Box<dyn std::error::Error>> {
        let cache_dir = Self::get_path(env, activity, "getCacheDir")?;
        let data_dir = Self::get_path(env, activity, "getFilesDir")?;
        let native_library_dir = Self::get_native_library_dir(env, activity)?;

        {
            let mut context = APPLICATION_CONTEXT
                .write()
                .map_err(|e| format!("Failed to write application context: {e}"))?;
            *context = Some(ApplicationContext {
                cache_dir,
                data_dir,
                native_library_dir,
            });
            if let Some(ref ctx) = *context {
                log::info!("ApplicationContext initialized: {:?}", ctx);
            }
        }
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

static APPLICATION_CONTEXT: RwLock<Option<ApplicationContext>> = RwLock::new(None);

pub fn get_application_context() -> ApplicationContext {
    let guard = match APPLICATION_CONTEXT.read() {
        Ok(g) => g,
        Err(e) => {
            panic!("Failed to read application context: {e}");
        }
    };
    match guard.as_ref() {
        Some(ctx) => ctx.clone(),
        None => {
            panic!("ApplicationContext is not initialized");
        }
    }
}
