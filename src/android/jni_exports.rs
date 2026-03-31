//! JNI callback exports for Android Activities.
//!
//! These `#[unsafe(no_mangle)] extern "system"` functions are called from Java
//! on the UI thread and forward events to the compositor via [`send_event`].

use jni::objects::JObject;
use jni::JNIEnv;

use crate::android::window_manager::{send_event, set_vulkan_rendering, use_vulkan_rendering, zero_copy_enabled, WindowEvent};

// ============================================================
// DebugActivity JNI callbacks
// ============================================================

/// JNI callback: toggle render mode from DebugActivity.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_phiresky_wayland_1android_DebugActivity_nativeSetVulkanRendering(
    _env: JNIEnv,
    _class: JObject,
    enabled: jni::sys::jboolean,
) {
    let val = enabled != 0;
    tracing::info!("Render mode toggled: {}", if val { "Vulkan" } else { "GLES" });
    set_vulkan_rendering(val);
}

/// JNI callback: get current render mode.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_phiresky_wayland_1android_DebugActivity_nativeGetVulkanRendering(
    _env: JNIEnv,
    _class: JObject,
) -> jni::sys::jboolean {
    if use_vulkan_rendering() { 1 } else { 0 }
}

/// JNI callback: toggle zero-copy from DebugActivity.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_phiresky_wayland_1android_DebugActivity_nativeSetZeroCopyEnabled(
    _env: JNIEnv,
    _class: JObject,
    enabled: jni::sys::jboolean,
) {
    let val = enabled != 0;
    tracing::info!("Zero-copy toggled: {}", if val { "ON" } else { "OFF" });
    #[cfg(feature = "zero-copy")]
    crate::android::window_manager::set_zero_copy(val);
}

/// JNI callback: get zero-copy state.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_phiresky_wayland_1android_DebugActivity_nativeGetZeroCopyEnabled(
    _env: JNIEnv,
    _class: JObject,
) -> jni::sys::jboolean {
    if zero_copy_enabled() { 1 } else { 0 }
}

/// JNI callback: toggle PipeWire from MainActivity (restoring saved preference).
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_phiresky_wayland_1android_MainActivity_nativeSetPipewireEnabled(
    _env: JNIEnv,
    _class: JObject,
    enabled: jni::sys::jboolean,
) {
    let val = enabled != 0;
    tracing::info!("PipeWire preference restored: {}", if val { "enabled" } else { "disabled" });
    crate::core::config::set_pipewire_enabled(val);
}

/// JNI callback: toggle PipeWire from DebugActivity.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_phiresky_wayland_1android_DebugActivity_nativeSetPipewireEnabled(
    _env: JNIEnv,
    _class: JObject,
    enabled: jni::sys::jboolean,
) {
    let val = enabled != 0;
    tracing::info!("PipeWire toggled: {}", if val { "enabled" } else { "disabled" });
    crate::core::config::set_pipewire_enabled(val);
}

/// JNI callback: get PipeWire enabled state.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_phiresky_wayland_1android_DebugActivity_nativeGetPipewireEnabled(
    _env: JNIEnv,
    _class: JObject,
) -> jni::sys::jboolean {
    if crate::core::config::pipewire_enabled() { 1 } else { 0 }
}

/// JNI callback: get debug log buffer for DebugActivity.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_phiresky_wayland_1android_DebugActivity_nativeGetDebugLog(
    env: JNIEnv,
    _class: JObject,
) -> jni::sys::jstring {
    let log = crate::android::utils::android_tracing::get_debug_log();
    match env.new_string(&log).or_else(|_| env.new_string("")) {
        Ok(s) => s.into_raw(),
        Err(e) => {
            tracing::error!("JNI new_string failed: {e}");
            std::ptr::null_mut()
        }
    }
}

// ============================================================
// WaylandWindowActivity JNI callbacks
// ============================================================

#[unsafe(no_mangle)]
extern "system" fn Java_io_github_phiresky_wayland_1android_WaylandWindowActivity_nativeSurfaceCreated(
    env: JNIEnv,
    _class: JObject,
    window_id: i32,
    surface: JObject,
) {
    let native_window = unsafe {
        ndk_sys::ANativeWindow_fromSurface(env.get_raw() as *mut _, surface.as_raw())
    };
    if !native_window.is_null() {
        unsafe { ndk_sys::ANativeWindow_acquire(native_window) };
        tracing::info!("JNI: surfaceCreated window_id={}", window_id);
        send_event(WindowEvent::SurfaceCreated {
            window_id: window_id as u32,
            native_window,
        });
    }
}

#[unsafe(no_mangle)]
extern "system" fn Java_io_github_phiresky_wayland_1android_WaylandWindowActivity_nativeSurfaceChanged(
    _env: JNIEnv,
    _class: JObject,
    window_id: i32,
    width: i32,
    height: i32,
) {
    tracing::info!("JNI: surfaceChanged window_id={} {}x{}", window_id, width, height);
    send_event(WindowEvent::SurfaceChanged {
        window_id: window_id as u32,
        width,
        height,
    });
}

#[unsafe(no_mangle)]
extern "system" fn Java_io_github_phiresky_wayland_1android_WaylandWindowActivity_nativeSurfaceDestroyed(
    _env: JNIEnv,
    _class: JObject,
    window_id: i32,
) {
    tracing::info!("JNI: surfaceDestroyed window_id={}", window_id);
    send_event(WindowEvent::SurfaceDestroyed {
        window_id: window_id as u32,
    });
}

#[unsafe(no_mangle)]
extern "system" fn Java_io_github_phiresky_wayland_1android_WaylandWindowActivity_nativeWindowClosed(
    _env: JNIEnv,
    _class: JObject,
    window_id: i32,
    is_finishing: bool,
) {
    tracing::info!("JNI: windowClosed window_id={} is_finishing={}", window_id, is_finishing);
    send_event(WindowEvent::WindowClosed {
        window_id: window_id as u32,
        is_finishing,
    });
}

/// JNI callback: user requested close (back button, DeX X). Send XDG close to client.
#[unsafe(no_mangle)]
extern "system" fn Java_io_github_phiresky_wayland_1android_WaylandWindowActivity_nativeRequestClose(
    _env: JNIEnv,
    _class: JObject,
    window_id: i32,
) {
    tracing::info!("JNI: closeRequested window_id={}", window_id);
    send_event(WindowEvent::CloseRequested {
        window_id: window_id as u32,
    });
}

#[unsafe(no_mangle)]
extern "system" fn Java_io_github_phiresky_wayland_1android_WaylandWindowActivity_nativeOnTouchEvent(
    _env: JNIEnv,
    _class: JObject,
    window_id: i32,
    action: i32,
    x: f32,
    y: f32,
) -> bool {
    send_event(WindowEvent::Touch {
        window_id: window_id as u32,
        action,
        x,
        y,
    });
    true
}

#[unsafe(no_mangle)]
extern "system" fn Java_io_github_phiresky_wayland_1android_WaylandWindowActivity_nativeOnKeyEvent(
    _env: JNIEnv,
    _class: JObject,
    window_id: i32,
    key_code: i32,
    action: i32,
    meta_state: i32,
) -> bool {
    // Only handle keys we can map; return false for unmapped keys so Android
    // can handle them (volume, home, etc.).
    if crate::android::backend::keymap::android_keycode_to_smithay(key_code).is_none() {
        return false;
    }
    send_event(WindowEvent::Key {
        window_id: window_id as u32,
        key_code,
        action,
        meta_state,
    });
    true
}

#[unsafe(no_mangle)]
extern "system" fn Java_io_github_phiresky_wayland_1android_WaylandWindowActivity_nativeOnImeText(
    mut env: JNIEnv,
    _class: JObject,
    window_id: i32,
    ime_type: i32,
    text: jni::objects::JString,
    delete_before: i32,
    delete_after: i32,
) {
    let text = crate::android::utils::jni_context::get_string(&mut env, &text);
    let event = match ime_type {
        0 => WindowEvent::ImeComposing { window_id: window_id as u32, text },
        1 => WindowEvent::ImeCommit { window_id: window_id as u32, text },
        2 => WindowEvent::ImeDelete { window_id: window_id as u32, before: delete_before, after: delete_after, text },
        3 => WindowEvent::ImeRecompose { window_id: window_id as u32, text },
        _ => return,
    };
    send_event(event);
}

#[unsafe(no_mangle)]
extern "system" fn Java_io_github_phiresky_wayland_1android_WaylandWindowActivity_nativeRightClick(
    _env: JNIEnv,
    _class: JObject,
    window_id: i32,
    x: f32,
    y: f32,
) {
    send_event(WindowEvent::RightClick {
        window_id: window_id as u32,
        x,
        y,
    });
}
