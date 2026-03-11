use jni::sys::JNIInvokeInterface_;
use jni::{JNIEnv, JavaVM};
use winit::platform::android::activity::AndroidApp;

/// A higher-order function to run a provided JNI function within the JVM context.
pub fn run_in_jvm<F, T>(jni_function: F, android_app: AndroidApp) -> Result<T, jni::errors::Error>
where
    F: FnOnce(&mut JNIEnv, &AndroidApp) -> T,
{
    let vm =
        unsafe { JavaVM::from_raw(android_app.vm_as_ptr() as *mut *const JNIInvokeInterface_) }?;

    let mut env = vm.attach_current_thread()?;

    let res = jni_function(&mut env, &android_app);

    unsafe { vm.detach_current_thread() };

    Ok(res)
}
