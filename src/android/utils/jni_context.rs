use jni::objects::{GlobalRef, JObject};
use jni::{JNIEnv, JavaVM};
use std::sync::OnceLock;

/// Global JNI context — stores the JavaVM and a global reference to the MainActivity.
/// Initialized once from `JNI_OnLoad` (VM) and `nativeInit` (activity).
struct JniContext {
    vm: JavaVM,
    activity: GlobalRef,
}

static CONTEXT: OnceLock<JniContext> = OnceLock::new();

/// Store the JavaVM pointer. Called from `JNI_OnLoad`.
static VM: OnceLock<JavaVM> = OnceLock::new();

/// Cache the JavaVM from `JNI_OnLoad`.
pub fn set_vm(vm: JavaVM) {
    let _ = VM.set(vm);
}

/// Initialize the full context with the Activity reference.
/// Must be called after `set_vm`.
pub fn init(env: &mut JNIEnv, activity: &JObject) {
    let vm = match VM.get() {
        Some(vm) => {
            // JavaVM::from_raw creates a new wrapper without ownership.
            // We need a clone-like operation — just recreate from the raw pointer.
            match unsafe { JavaVM::from_raw(vm.get_java_vm_pointer()) } {
                Ok(vm) => vm,
                Err(e) => {
                    log::error!("Failed to recreate JavaVM: {e}");
                    return;
                }
            }
        }
        None => {
            log::error!("JNI context init called before JNI_OnLoad");
            return;
        }
    };

    let activity_ref = match env.new_global_ref(activity) {
        Ok(r) => r,
        Err(e) => {
            log::error!("Failed to create global ref for activity: {e}");
            return;
        }
    };

    let _ = CONTEXT.set(JniContext {
        vm,
        activity: activity_ref,
    });
}

/// Get the cached JavaVM.
pub fn vm() -> &'static JavaVM {
    match CONTEXT.get() {
        Some(ctx) => &ctx.vm,
        None => match VM.get() {
            Some(vm) => vm,
            None => panic!("JNI context not initialized"),
        },
    }
}

/// Get a JNIEnv for the current thread.
pub fn attach_env() -> jni::AttachGuard<'static> {
    match vm().attach_current_thread() {
        Ok(env) => env,
        Err(e) => panic!("Failed to attach JNI env: {e}"),
    }
}

/// Get the global Activity reference.
pub fn activity() -> &'static GlobalRef {
    match CONTEXT.get() {
        Some(ctx) => &ctx.activity,
        None => panic!("JNI context not initialized — nativeInit not called yet"),
    }
}

/// Run a closure with a JNIEnv and the Activity object.
/// This is the primary way to make JNI calls throughout the codebase.
pub fn with_jni<F, R>(f: F) -> Result<R, jni::errors::Error>
where
    F: FnOnce(&mut JNIEnv, &JObject) -> Result<R, jni::errors::Error>,
{
    let ctx = match CONTEXT.get() {
        Some(ctx) => ctx,
        None => {
            return Err(jni::errors::Error::JniCall(jni::errors::JniError::Other(1)));
        }
    };
    let mut env = ctx.vm.attach_current_thread()?;
    f(&mut env, ctx.activity.as_obj())
}
