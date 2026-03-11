use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::mpsc;
use std::sync::Mutex;

use jni::objects::{JObject, JValue};
use jni::sys::jobject;
use jni::JNIEnv;
use smithay::backend::egl::EGLSurface;
use smithay::utils::{Physical, Size};
use smithay::wayland::shell::xdg::ToplevelSurface;
use winit::platform::android::activity::AndroidApp;
use winit::raw_window_handle::AndroidNdkWindowHandle;

// FFI for ANativeWindow
unsafe extern "C" {
    fn ANativeWindow_fromSurface(env: *mut jni::sys::JNIEnv, surface: jobject) -> *mut c_void;
    fn ANativeWindow_acquire(window: *mut c_void);
    fn ANativeWindow_release(window: *mut c_void);
}

/// Events sent from JNI callbacks (UI thread) to the compositor (render thread).
pub enum WindowEvent {
    SurfaceCreated { window_id: u32, native_window: *mut c_void },
    SurfaceChanged { window_id: u32, width: i32, height: i32 },
    SurfaceDestroyed { window_id: u32 },
    WindowClosed { window_id: u32 },
    Touch { window_id: u32, action: i32, x: f32, y: f32 },
    Key { window_id: u32, key_code: i32, action: i32, meta_state: i32 },
}

unsafe impl Send for WindowEvent {}

/// Global channel sender for JNI callbacks to post events.
static EVENT_SENDER: Mutex<Option<mpsc::Sender<WindowEvent>>> = Mutex::new(None);

/// State for a single window (one per XDG toplevel).
pub struct WindowState {
    pub window_id: u32,
    pub toplevel: ToplevelSurface,
    pub native_window: Option<*mut c_void>,
    pub egl_surface: Option<EGLSurface>,
    pub size: Size<i32, Physical>,
    pub needs_redraw: bool,
}

/// Manages the mapping between XDG toplevels and Android Activities.
pub struct WindowManager {
    pub windows: HashMap<u32, WindowState>,
    pub event_rx: mpsc::Receiver<WindowEvent>,
    next_id: u32,
    android_app: AndroidApp,
}

impl WindowManager {
    pub fn new(android_app: AndroidApp) -> Self {
        let (tx, rx) = mpsc::channel();
        match EVENT_SENDER.lock() {
            Ok(mut guard) => *guard = Some(tx),
            Err(e) => log::error!("Failed to lock EVENT_SENDER: {e}"),
        }

        Self {
            windows: HashMap::new(),
            event_rx: rx,
            next_id: 1,
            android_app,
        }
    }

    /// Called when smithay creates a new XDG toplevel.
    /// Allocates a window ID and launches a new Android Activity.
    pub fn new_toplevel(&mut self, toplevel: ToplevelSurface) -> u32 {
        let window_id = self.next_id;
        self.next_id += 1;

        self.windows.insert(window_id, WindowState {
            window_id,
            toplevel,
            native_window: None,
            egl_surface: None,
            size: (0, 0).into(),
            needs_redraw: true,
        });

        self.launch_activity(window_id);
        window_id
    }

    /// Launch a WaylandWindowActivity via JNI with the given window_id.
    fn launch_activity(&self, window_id: u32) {
        if let Err(e) = self.launch_activity_inner(window_id) {
            log::error!("Failed to launch Activity for window_id={}: {}", window_id, e);
        }
    }

    fn launch_activity_inner(&self, window_id: u32) -> Result<(), jni::errors::Error> {
        let vm = unsafe {
            jni::JavaVM::from_raw(self.android_app.vm_as_ptr() as *mut _)
        }?;
        let mut env = vm.attach_current_thread()?;

        let activity = unsafe {
            JObject::from_raw(self.android_app.activity_as_ptr() as *mut _)
        };

        // Use the Activity's classloader (not the system one) to find our Java class.
        // env.find_class() uses the system classloader which doesn't know about app classes.
        let class_loader = env
            .call_method(&activity, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])?
            .l()?;
        let class_name = env
            .new_string("io.github.phiresky.wayland_android.WaylandWindowActivity")?;
        let activity_class = env
            .call_method(
                &class_loader,
                "loadClass",
                "(Ljava/lang/String;)Ljava/lang/Class;",
                &[JValue::Object(&class_name)],
            )?
            .l()?;

        // Create Intent for WaylandWindowActivity
        let intent_class = env.find_class("android/content/Intent")?;
        let intent = env.new_object(
            &intent_class,
            "(Landroid/content/Context;Ljava/lang/Class;)V",
            &[
                JValue::Object(&activity),
                JValue::Object(&activity_class),
            ],
        )?;

        // Put window_id as extra
        let key = env.new_string("window_id")?;
        env.call_method(
            &intent,
            "putExtra",
            "(Ljava/lang/String;I)Landroid/content/Intent;",
            &[JValue::Object(&key), JValue::Int(window_id as i32)],
        )?;

        // Add FLAG_ACTIVITY_NEW_DOCUMENT | FLAG_ACTIVITY_MULTIPLE_TASK
        // so each window appears as a separate task in recents
        let flags: i32 = 0x00080000 | 0x08000000; // NEW_DOCUMENT | MULTIPLE_TASK
        env.call_method(
            &intent,
            "addFlags",
            "(I)Landroid/content/Intent;",
            &[JValue::Int(flags)],
        )?;

        // Start the activity
        env.call_method(
            &activity,
            "startActivity",
            "(Landroid/content/Intent;)V",
            &[JValue::Object(&intent)],
        )?;

        log::info!("Launched WaylandWindowActivity for window_id={}", window_id);

        // Prevent JNI ref leak
        unsafe { vm.detach_current_thread() };
        Ok(())
    }

    /// Remove a window and clean up its resources.
    pub fn remove_window(&mut self, window_id: u32) {
        if let Some(state) = self.windows.remove(&window_id) {
            if let Some(native_window) = state.native_window {
                unsafe { ANativeWindow_release(native_window) };
            }
            log::info!("Removed window_id={}", window_id);
        }
    }

    /// Get the ANativeWindow handle for creating an EGL surface.
    pub fn get_native_handle(&self, window_id: u32) -> Option<AndroidNdkWindowHandle> {
        self.windows.get(&window_id).and_then(|w| {
            w.native_window.and_then(|ptr| {
                NonNull::new(ptr).map(AndroidNdkWindowHandle::new)
            })
        })
    }
}

// ============================================================
// JNI exports — called from WaylandWindowActivity on UI thread
// ============================================================

fn send_event(event: WindowEvent) {
    match EVENT_SENDER.lock() {
        Ok(guard) => {
            if let Some(tx) = guard.as_ref() {
                let _ = tx.send(event);
            }
        }
        Err(e) => log::error!("Failed to lock EVENT_SENDER: {e}"),
    }
}

#[unsafe(no_mangle)]
extern "system" fn Java_io_github_phiresky_wayland_1android_WaylandWindowActivity_nativeSurfaceCreated(
    env: JNIEnv,
    _class: JObject,
    window_id: i32,
    surface: JObject,
) {
    let native_window = unsafe {
        ANativeWindow_fromSurface(env.get_raw() as *mut _, surface.as_raw())
    };
    if !native_window.is_null() {
        unsafe { ANativeWindow_acquire(native_window) };
        log::info!("JNI: surfaceCreated window_id={}", window_id);
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
    log::info!("JNI: surfaceChanged window_id={} {}x{}", window_id, width, height);
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
    log::info!("JNI: surfaceDestroyed window_id={}", window_id);
    send_event(WindowEvent::SurfaceDestroyed {
        window_id: window_id as u32,
    });
}

#[unsafe(no_mangle)]
extern "system" fn Java_io_github_phiresky_wayland_1android_WaylandWindowActivity_nativeWindowClosed(
    _env: JNIEnv,
    _class: JObject,
    window_id: i32,
) {
    log::info!("JNI: windowClosed window_id={}", window_id);
    send_event(WindowEvent::WindowClosed {
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
    send_event(WindowEvent::Key {
        window_id: window_id as u32,
        key_code,
        action,
        meta_state,
    });
    true
}
