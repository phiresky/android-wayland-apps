use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::mpsc;
use std::sync::Mutex;
use std::time::Instant;

use jni::objects::JValue;
use smithay::backend::egl::EGLSurface;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Physical, Size};
use smithay::wayland::shell::wlr_layer::LayerSurface;
use smithay::wayland::shell::xdg::ToplevelSurface;
use raw_window_handle::AndroidNdkWindowHandle;

use std::os::unix::io::RawFd;
use crate::android::backend::signal_wake;
use crate::android::backend::surface_transaction::SurfaceControlHandle;
use crate::android::backend::vulkan_renderer::AhbTarget;

use ndk_sys::ANativeWindow;

/// Events sent from JNI callbacks (UI thread) to the compositor (render thread).
pub enum WindowEvent {
    SurfaceCreated { window_id: u32, native_window: *mut ANativeWindow },
    SurfaceChanged { window_id: u32, width: i32, height: i32 },
    SurfaceDestroyed { window_id: u32 },
    WindowClosed { window_id: u32, is_finishing: bool },
    CloseRequested { window_id: u32 },
    Touch { window_id: u32, action: i32, x: f32, y: f32 },
    Key { window_id: u32, key_code: i32, action: i32, meta_state: i32 },
    RightClick { window_id: u32, x: f32, y: f32 },
    ImeComposing { window_id: u32, text: String },
    ImeCommit { window_id: u32, text: String },
    ImeDelete { window_id: u32, before: i32, after: i32, text: String },
    ImeRecompose { window_id: u32, text: String },
    /// XDG Desktop Portal file chooser request from portal bridge.
    PortalRequest(crate::android::portal::PortalRequest),
}

unsafe impl Send for WindowEvent {}

/// Global channel sender for JNI callbacks to post events.
static EVENT_SENDER: Mutex<Option<mpsc::Sender<WindowEvent>>> = Mutex::new(None);

/// Global eventfd for waking the compositor thread from JNI callbacks.
static WAKE_FD: Mutex<Option<RawFd>> = Mutex::new(None);

/// The kind of Wayland shell surface backing a window.
pub enum SurfaceKind {
    Toplevel(ToplevelSurface),
    Layer(LayerSurface),
}

impl SurfaceKind {
    pub fn wl_surface(&self) -> &WlSurface {
        match self {
            SurfaceKind::Toplevel(t) => t.wl_surface(),
            SurfaceKind::Layer(l) => l.wl_surface(),
        }
    }

    pub fn send_close(&self) {
        match self {
            SurfaceKind::Toplevel(t) => t.send_close(),
            SurfaceKind::Layer(l) => l.send_close(),
        }
    }
}

/// State for a single window (one per XDG toplevel or layer surface).
pub struct WindowState {
    pub window_id: u32,
    pub surface_kind: SurfaceKind,
    pub size: Size<i32, Physical>,
    pub needs_redraw: bool,
    /// The client's preferred logical size from its initial geometry commit.
    /// DeX enforces a minimum window height larger than small dialogs need,
    /// so we cap the Wayland configure to this size and center the content.
    pub preferred_size: Option<Size<i32, Logical>>,
    /// Android surface handles and rendering backend state.
    pub render: RenderSurfaces,
    /// Activity lifecycle tracking.
    pub lifecycle: WindowLifecycle,
    /// Frame performance and debug metrics.
    pub metrics: FrameMetrics,
}

/// Android surface handles and rendering backend state for a window.
pub struct RenderSurfaces {
    pub native_window: Option<*mut ANativeWindow>,
    pub egl_surface: Option<EGLSurface>,
    /// AHB + ASurfaceTransaction path for dmabuf compositing.
    pub ahb_surface: Option<AhbWindowSurface>,
    /// Render mode for this window's client, detected from client env vars.
    /// `None` means not yet checked.
    pub render_mode: Option<RenderMode>,
    /// Compositor-allocated AHB for this window (server-side allocation path).
    /// When present, the committed dmabuf can be presented directly via
    /// ASurfaceTransaction without any GPU blit.
    pub server_ahb: Option<std::sync::Arc<crate::android::backend::surface_transaction::HardwareBuffer>>,
}

/// Activity lifecycle tracking for a window.
pub struct WindowLifecycle {
    /// Whether the Android Activity has been launched for this window.
    /// We delay launch until the client commits so we can use setLaunchBounds
    /// to size the DeX freeform window correctly.
    pub activity_launched: bool,
    /// When this window was created. Used for fallback launch timeout.
    pub created_time: Instant,
    /// When a close was requested but the Activity was already destroyed.
    /// Used to delay relaunching until the client has had time to respond.
    pub close_pending_since: Option<Instant>,
}

/// Frame performance and debug metrics for a window.
pub struct FrameMetrics {
    /// Frames rendered since last FPS sample.
    pub frame_count: u32,
    /// Last render method used for this window (for debug overlay).
    pub last_render_method: &'static str,
    /// Last frame's commit-to-present time in microseconds (for perf comparison).
    pub last_frame_us: u64,
    /// Last buffer size committed by the client.
    pub last_buffer_size: Option<(u32, u32)>,
    /// VK shm-gpu import failed for this window — don't retry every frame.
    /// Set when vkAllocateMemory(import) fails for the shm pool fd.
    pub vk_shm_gpu_failed: bool,
    /// Count of consecutive frames where shm data was unreadable (zeros).
    /// Used to delay GLES fallback — CPU clients (gedit) become readable
    /// after 1-2 frames, GPU clients (Firefox/Zink) stay at zero forever.
    pub shm_zero_frames: u32,
}

/// Per-window ASurfaceTransaction state.
pub struct AhbWindowSurface {
    pub surface_control: SurfaceControlHandle,
    /// Vulkan blit target. None for zero-copy compositor-allocated AHB path.
    pub ahb_target: Option<AhbTarget>,
    /// True while SurfaceFlinger is displaying the previous frame.
    /// Cleared by OnComplete callback — prevents rendering faster than vsync.
    pub frame_in_flight: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// How the compositor should render a client's buffers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderMode {
    /// Zero-copy Vulkan blit for dmabuf surfaces (default).
    Vulkan,
    /// GLES compositing with CPU readback (fallback / debug).
    Gles,
}

/// Global toggle: true = Vulkan (default), false = GLES.
/// Toggled from DebugActivity via JNI.
static USE_VULKAN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Global toggle: zero-copy AHB present. Default OFF (blit path is correct).
#[cfg(feature = "zero-copy")]
static USE_ZERO_COPY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn zero_copy_enabled() -> bool {
    #[cfg(feature = "zero-copy")]
    { USE_ZERO_COPY.load(std::sync::atomic::Ordering::Relaxed) }
    #[cfg(not(feature = "zero-copy"))]
    { false }
}

pub fn use_vulkan_rendering() -> bool {
    USE_VULKAN.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn set_vulkan_rendering(enabled: bool) {
    USE_VULKAN.store(enabled, std::sync::atomic::Ordering::Relaxed);
}

#[cfg(feature = "zero-copy")]
pub fn set_zero_copy(enabled: bool) {
    USE_ZERO_COPY.store(enabled, std::sync::atomic::Ordering::Relaxed);
}

/// Manages the mapping between XDG toplevels and Android Activities.
pub struct WindowManager {
    pub windows: HashMap<u32, WindowState>,
    pub event_rx: mpsc::Receiver<WindowEvent>,
    next_id: u32,
}

impl WindowManager {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        if let Ok(mut guard) = EVENT_SENDER.lock() {
            *guard = Some(tx);
        }

        Self {
            windows: HashMap::new(),
            event_rx: rx,
            next_id: 1,
        }
    }

    /// Allocates a window ID for the surface. The Activity is NOT launched yet —
    /// we wait for the client's first commit so we can use setLaunchBounds.
    pub(crate) fn new_window(&mut self, surface_kind: SurfaceKind) -> u32 {
        let window_id = self.next_id;
        self.next_id += 1;

        self.windows.insert(window_id, WindowState {
            window_id,
            surface_kind,
            size: (0, 0).into(),
            needs_redraw: true,
            preferred_size: None,
            render: RenderSurfaces {
                native_window: None,
                egl_surface: None,
                ahb_surface: None,
                render_mode: None,
                server_ahb: None,
            },
            lifecycle: WindowLifecycle {
                activity_launched: false,
                created_time: Instant::now(),
                close_pending_since: None,
            },
            metrics: FrameMetrics {
                frame_count: 0,
                last_render_method: "none",
                last_frame_us: 0,
                last_buffer_size: None,
                vk_shm_gpu_failed: false,
                shm_zero_frames: 0,
            },
        });

        window_id
    }

    /// Launch a WaylandWindowActivity via JNI with the given window_id.
    /// If bounds are provided (physical pixels), uses setLaunchBounds for DeX freeform sizing.
    pub fn launch_activity(&mut self, window_id: u32, bounds: Option<(i32, i32)>) {
        if let Some(window) = self.windows.get_mut(&window_id) {
            window.lifecycle.activity_launched = true;
        }
        if let Err(e) = Self::launch_activity_inner(window_id, bounds) {
            tracing::error!("Failed to launch Activity for window_id={}: {}", window_id, e);
        }
    }

    fn launch_activity_inner(window_id: u32, bounds: Option<(i32, i32)>) -> Result<(), jni::errors::Error> {
        crate::android::utils::jni_context::with_jni(|env, activity| {
            let activity_class = crate::android::utils::jni_context::load_class(
                env, activity, "io.github.phiresky.wayland_android.WaylandWindowActivity",
            )?;

            // Create Intent for WaylandWindowActivity
            let intent_class = env.find_class("android/content/Intent")?;
            let intent = env.new_object(
                &intent_class,
                "(Landroid/content/Context;Ljava/lang/Class;)V",
                &[
                    JValue::Object(activity),
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

            // Each window appears as a separate task in recents.
            const FLAG_ACTIVITY_NEW_DOCUMENT: i32 = 0x00080000;
            const FLAG_ACTIVITY_MULTIPLE_TASK: i32 = 0x08000000;
            let flags: i32 = FLAG_ACTIVITY_NEW_DOCUMENT | FLAG_ACTIVITY_MULTIPLE_TASK;
            env.call_method(
                &intent,
                "addFlags",
                "(I)Landroid/content/Intent;",
                &[JValue::Int(flags)],
            )?;

            // Build ActivityOptions with launch bounds if provided.
            let bundle = if let Some((w, h)) = bounds {
                let options_class = env.find_class("android/app/ActivityOptions")?;
                let options = env.call_static_method(
                    &options_class,
                    "makeBasic",
                    "()Landroid/app/ActivityOptions;",
                    &[],
                )?.l()?;
                // Create Rect(0, 0, w, h) for launch bounds
                let rect_class = env.find_class("android/graphics/Rect")?;
                let rect = env.new_object(
                    &rect_class,
                    "(IIII)V",
                    &[JValue::Int(0), JValue::Int(0), JValue::Int(w), JValue::Int(h)],
                )?;
                env.call_method(
                    &options,
                    "setLaunchBounds",
                    "(Landroid/graphics/Rect;)Landroid/app/ActivityOptions;",
                    &[JValue::Object(&rect)],
                )?;
                let bundle = env.call_method(
                    &options,
                    "toBundle",
                    "()Landroid/os/Bundle;",
                    &[],
                )?.l()?;
                Some(bundle)
            } else {
                None
            };

            // Start the activity (with or without bounds)
            if let Some(bundle) = bundle {
                env.call_method(
                    activity,
                    "startActivity",
                    "(Landroid/content/Intent;Landroid/os/Bundle;)V",
                    &[JValue::Object(&intent), JValue::Object(&bundle)],
                )?;
                tracing::info!("Launched WaylandWindowActivity for window_id={} with bounds {:?}", window_id, bounds);
            } else {
                env.call_method(
                    activity,
                    "startActivity",
                    "(Landroid/content/Intent;)V",
                    &[JValue::Object(&intent)],
                )?;
                tracing::info!("Launched WaylandWindowActivity for window_id={} (no bounds)", window_id);
            }
            Ok(())
        })
    }

    /// Find the window ID for a given Wayland surface.
    pub fn find_window_id(&self, predicate: impl Fn(&SurfaceKind) -> bool) -> Option<u32> {
        self.windows.iter().find_map(|(id, w)| predicate(&w.surface_kind).then_some(*id))
    }

    /// Remove a window and clean up its resources.
    pub fn remove_window(&mut self, window_id: u32) {
        if let Some(state) = self.windows.remove(&window_id) {
            if let Some(native_window) = state.render.native_window {
                unsafe { ndk_sys::ANativeWindow_release(native_window) };
            }
            tracing::info!("Removed window_id={}", window_id);
        }
    }

    /// Get the ANativeWindow handle for creating an EGL surface.
    pub fn get_native_handle(&self, window_id: u32) -> Option<AndroidNdkWindowHandle> {
        self.windows.get(&window_id).and_then(|w| {
            w.render.native_window.and_then(|ptr| {
                NonNull::new(ptr as *mut c_void).map(AndroidNdkWindowHandle::new)
            })
        })
    }
}

/// Set the eventfd so JNI callbacks can wake the compositor thread.
pub fn set_wake_fd(fd: RawFd) {
    if let Ok(mut guard) = WAKE_FD.lock() {
        *guard = Some(fd);
    }
}

pub fn send_event(event: WindowEvent) {
    if let Ok(guard) = EVENT_SENDER.lock()
        && let Some(tx) = guard.as_ref()
    {
        let _ = tx.send(event);
    }
    // Wake the compositor thread so it processes the event promptly.
    if let Ok(guard) = WAKE_FD.lock()
        && let Some(&fd) = guard.as_ref()
    {
        signal_wake(fd);
    }
}
