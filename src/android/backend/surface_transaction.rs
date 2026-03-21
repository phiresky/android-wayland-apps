//! RAII wrappers for Android ASurfaceControl, ASurfaceTransaction,
//! and AHardwareBuffer APIs. Uses ndk-sys for FFI bindings.

use std::ffi::{c_void, CString};
use std::os::unix::io::RawFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ndk_sys::{
    AHardwareBuffer, AHardwareBuffer_Desc, ARect,
    ASurfaceControl, ASurfaceTransaction, ASurfaceTransactionStats,
    ANativeWindow,
};

// ── RAII Wrappers ──────────────────────────────────────────────────────────

/// Owns an `ASurfaceControl*`, releases on drop.
pub struct SurfaceControlHandle {
    ptr: *mut ASurfaceControl,
}

unsafe impl Send for SurfaceControlHandle {}

impl SurfaceControlHandle {
    /// Create a child surface control from an ANativeWindow.
    pub fn from_window(native_window: *mut ANativeWindow, name: &str) -> Option<Self> {
        let c_name = CString::new(name).ok()?;
        let ptr = unsafe { ndk_sys::ASurfaceControl_createFromWindow(native_window, c_name.as_ptr()) };
        if ptr.is_null() {
            tracing::error!("ASurfaceControl_createFromWindow returned null");
            None
        } else {
            tracing::info!("Created ASurfaceControl {:?} for {:?}", ptr, name);
            Some(Self { ptr })
        }
    }

    pub fn as_ptr(&self) -> *mut ASurfaceControl {
        self.ptr
    }
}

impl Drop for SurfaceControlHandle {
    fn drop(&mut self) {
        tracing::info!("Releasing ASurfaceControl {:?}", self.ptr);
        unsafe { ndk_sys::ASurfaceControl_release(self.ptr) };
    }
}

/// Owns an `AHardwareBuffer*`, releases on drop.
pub struct HardwareBuffer {
    ptr: *mut AHardwareBuffer,
}

unsafe impl Send for HardwareBuffer {}

impl HardwareBuffer {
    /// Allocate a new AHardwareBuffer.
    pub fn allocate(width: u32, height: u32, format: u32, usage: u64) -> Option<Self> {
        let desc = AHardwareBuffer_Desc {
            width,
            height,
            layers: 1,
            format,
            usage,
            stride: 0,
            rfu0: 0,
            rfu1: 0,
        };
        let mut ptr: *mut AHardwareBuffer = std::ptr::null_mut();
        let ret = unsafe { ndk_sys::AHardwareBuffer_allocate(&desc, &mut ptr) };
        if ret != 0 || ptr.is_null() {
            tracing::error!("AHardwareBuffer_allocate failed: ret={}", ret);
            None
        } else {
            tracing::info!("Allocated AHardwareBuffer {:?} ({}x{} fmt={})", ptr, width, height, format);
            Some(Self { ptr })
        }
    }

    /// Get the raw pointer for Vulkan import and ASurfaceTransaction.
    pub fn as_ptr(&self) -> *mut AHardwareBuffer {
        self.ptr
    }
}

impl Drop for HardwareBuffer {
    fn drop(&mut self) {
        tracing::info!("Releasing AHardwareBuffer {:?}", self.ptr);
        unsafe { ndk_sys::AHardwareBuffer_release(self.ptr) };
    }
}

// ── Constants (re-exported from ndk-sys newtype wrappers) ──────────────────

pub const AHB_FORMAT_R8G8B8A8_UNORM: u32 =
    ndk_sys::AHardwareBuffer_Format::AHARDWAREBUFFER_FORMAT_R8G8B8A8_UNORM.0;
pub const AHB_USAGE_GPU_FRAMEBUFFER: u64 =
    ndk_sys::AHardwareBuffer_UsageFlags::AHARDWAREBUFFER_USAGE_GPU_FRAMEBUFFER.0;
pub const AHB_USAGE_GPU_SAMPLED_IMAGE: u64 =
    ndk_sys::AHardwareBuffer_UsageFlags::AHARDWAREBUFFER_USAGE_GPU_SAMPLED_IMAGE.0;
pub const AHB_USAGE_COMPOSER_OVERLAY: u64 =
    ndk_sys::AHardwareBuffer_UsageFlags::AHARDWAREBUFFER_USAGE_COMPOSER_OVERLAY.0;

// ── OnComplete callback for vsync throttling ───────────────────────────────

struct OnCompleteData {
    flag: Arc<AtomicBool>,
    wake_fd: RawFd,
}

unsafe extern "C" fn on_complete_callback(
    context: *mut c_void,
    _stats: *mut ASurfaceTransactionStats,
) {
    let data = unsafe { Box::from_raw(context as *mut OnCompleteData) };
    data.flag.store(false, Ordering::Release);
    let val: u64 = 1;
    unsafe { libc::write(data.wake_fd, &val as *const u64 as *const libc::c_void, 8) };
}

/// Submit an AHardwareBuffer to SurfaceFlinger via ASurfaceTransaction.
/// Sets geometry to scale the buffer to fill the window (dest_w × dest_h).
/// Uses OnComplete callback to signal when the frame is displayed.
pub fn present_buffer(
    surface_control: &SurfaceControlHandle,
    buffer: &HardwareBuffer,
    acquire_fence_fd: i32,
    buf_w: u32,
    buf_h: u32,
    dest_w: i32,
    dest_h: i32,
    frame_in_flight: &Arc<AtomicBool>,
    wake_fd: RawFd,
) {
    let source = ARect { left: 0, top: 0, right: buf_w as i32, bottom: buf_h as i32 };
    let destination = ARect { left: 0, top: 0, right: dest_w, bottom: dest_h };

    let cb_data = Box::new(OnCompleteData {
        flag: frame_in_flight.clone(),
        wake_fd,
    });
    let cb_ctx = Box::into_raw(cb_data) as *mut c_void;

    frame_in_flight.store(true, Ordering::Release);

    unsafe {
        let txn = ndk_sys::ASurfaceTransaction_create();
        ndk_sys::ASurfaceTransaction_setBuffer(txn, surface_control.as_ptr(), buffer.as_ptr(), acquire_fence_fd);
        ndk_sys::ASurfaceTransaction_setGeometry(txn, surface_control.as_ptr(), &source, &destination, 0);
        ndk_sys::ASurfaceTransaction_setOnComplete(txn, cb_ctx, Some(on_complete_callback));
        ndk_sys::ASurfaceTransaction_apply(txn);
        ndk_sys::ASurfaceTransaction_delete(txn);
    }
}

/// Set visibility on a surface control (call once after creation).
pub fn set_visible(surface_control: &SurfaceControlHandle) {
    unsafe {
        let txn = ndk_sys::ASurfaceTransaction_create();
        ndk_sys::ASurfaceTransaction_setVisibility(
            txn,
            surface_control.as_ptr(),
            ndk_sys::ASurfaceTransactionVisibility::ASURFACE_TRANSACTION_VISIBILITY_SHOW,
        );
        ndk_sys::ASurfaceTransaction_apply(txn);
        ndk_sys::ASurfaceTransaction_delete(txn);
    }
}
