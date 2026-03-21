//! FFI bindings and RAII wrappers for Android ASurfaceControl, ASurfaceTransaction,
//! and AHardwareBuffer APIs.

use std::ffi::{c_char, c_void, CString};
use std::os::unix::io::RawFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

// ── NDK FFI ────────────────────────────────────────────────────────────────

type ASurfaceTransactionOnComplete =
    unsafe extern "C" fn(context: *mut c_void, stats: *mut c_void);

#[link(name = "android")]
unsafe extern "C" {
    fn ASurfaceControl_createFromWindow(
        parent: *mut c_void,
        debug_name: *const c_char,
    ) -> *mut c_void;
    fn ASurfaceControl_release(surface_control: *mut c_void);

    fn ASurfaceTransaction_create() -> *mut c_void;
    fn ASurfaceTransaction_delete(transaction: *mut c_void);
    fn ASurfaceTransaction_apply(transaction: *mut c_void) -> i32;
    fn ASurfaceTransaction_setBuffer(
        transaction: *mut c_void,
        surface_control: *mut c_void,
        buffer: *mut c_void,
        acquire_fence_fd: i32,
    );
    fn ASurfaceTransaction_setVisibility(
        transaction: *mut c_void,
        surface_control: *mut c_void,
        visibility: i8,
    );
    fn ASurfaceTransaction_setOnComplete(
        transaction: *mut c_void,
        context: *mut c_void,
        func: ASurfaceTransactionOnComplete,
    );
    fn ASurfaceTransaction_setGeometry(
        transaction: *mut c_void,
        surface_control: *mut c_void,
        source: *const ARect,
        destination: *const ARect,
        transform: i32,
    );

    fn AHardwareBuffer_allocate(
        desc: *const AHardwareBufferDesc,
        out_buffer: *mut *mut c_void,
    ) -> i32;
    fn AHardwareBuffer_release(buffer: *mut c_void);
    fn AHardwareBuffer_describe(buffer: *const c_void, out_desc: *mut AHardwareBufferDesc);
}

// ── ARect ──────────────────────────────────────────────────────────────────

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ARect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

// ── Constants ──────────────────────────────────────────────────────────────

/// AHARDWAREBUFFER_FORMAT_R8G8B8A8_UNORM
pub const AHB_FORMAT_R8G8B8A8_UNORM: u32 = 1;

/// AHARDWAREBUFFER_USAGE_GPU_FRAMEBUFFER (1ULL << 8)
pub const AHB_USAGE_GPU_FRAMEBUFFER: u64 = 1 << 8;
/// AHARDWAREBUFFER_USAGE_GPU_SAMPLED_IMAGE (1ULL << 9)
pub const AHB_USAGE_GPU_SAMPLED_IMAGE: u64 = 1 << 9;
/// AHARDWAREBUFFER_USAGE_COMPOSER_OVERLAY (1ULL << 11)
pub const AHB_USAGE_COMPOSER_OVERLAY: u64 = 1 << 11;

/// ASURFACE_TRANSACTION_VISIBILITY_SHOW
pub const VISIBILITY_SHOW: i8 = 1;

// ── AHardwareBufferDesc ────────────────────────────────────────────────────

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct AHardwareBufferDesc {
    pub width: u32,
    pub height: u32,
    pub layers: u32,
    pub format: u32,
    pub usage: u64,
    pub stride: u32,
    pub rfu0: u32,
    pub rfu1: u64,
}

// ── RAII Wrappers ──────────────────────────────────────────────────────────

/// Owns an `ASurfaceControl*`, releases on drop.
pub struct SurfaceControl {
    ptr: *mut c_void,
}

unsafe impl Send for SurfaceControl {}

impl SurfaceControl {
    /// Create a child surface control from an ANativeWindow.
    /// Returns `None` if `ASurfaceControl_createFromWindow` returns null.
    pub fn from_window(native_window: *mut c_void, name: &str) -> Option<Self> {
        let c_name = CString::new(name).ok()?;
        let ptr = unsafe { ASurfaceControl_createFromWindow(native_window, c_name.as_ptr()) };
        if ptr.is_null() {
            tracing::error!("ASurfaceControl_createFromWindow returned null");
            None
        } else {
            tracing::info!("Created ASurfaceControl {:?} for {:?}", ptr, name);
            Some(Self { ptr })
        }
    }

    pub fn as_ptr(&self) -> *mut c_void {
        self.ptr
    }
}

impl Drop for SurfaceControl {
    fn drop(&mut self) {
        tracing::info!("Releasing ASurfaceControl {:?}", self.ptr);
        unsafe { ASurfaceControl_release(self.ptr) };
    }
}

/// Owns an `AHardwareBuffer*`, releases on drop.
pub struct HardwareBuffer {
    ptr: *mut c_void,
}

unsafe impl Send for HardwareBuffer {}

impl HardwareBuffer {
    /// Allocate a new AHardwareBuffer.
    pub fn allocate(width: u32, height: u32, format: u32, usage: u64) -> Option<Self> {
        let desc = AHardwareBufferDesc {
            width,
            height,
            layers: 1,
            format,
            usage,
            stride: 0,
            rfu0: 0,
            rfu1: 0,
        };
        let mut ptr: *mut c_void = std::ptr::null_mut();
        let ret = unsafe { AHardwareBuffer_allocate(&desc, &mut ptr) };
        if ret != 0 || ptr.is_null() {
            tracing::error!("AHardwareBuffer_allocate failed: ret={}", ret);
            None
        } else {
            tracing::info!("Allocated AHardwareBuffer {:?} ({}x{} fmt={})", ptr, width, height, format);
            Some(Self { ptr })
        }
    }

    pub fn describe(&self) -> AHardwareBufferDesc {
        let mut desc = AHardwareBufferDesc {
            width: 0, height: 0, layers: 0, format: 0, usage: 0, stride: 0, rfu0: 0, rfu1: 0,
        };
        unsafe { AHardwareBuffer_describe(self.ptr, &mut desc) };
        desc
    }

    pub fn as_ptr(&self) -> *mut c_void {
        self.ptr
    }
}

impl Drop for HardwareBuffer {
    fn drop(&mut self) {
        tracing::info!("Releasing AHardwareBuffer {:?}", self.ptr);
        unsafe { AHardwareBuffer_release(self.ptr) };
    }
}

// ── OnComplete callback for vsync throttling ───────────────────────────────

struct OnCompleteData {
    flag: Arc<AtomicBool>,
    wake_fd: RawFd,
}

unsafe extern "C" fn on_complete_callback(context: *mut c_void, _stats: *mut c_void) {
    let data = unsafe { Box::from_raw(context as *mut OnCompleteData) };
    data.flag.store(false, Ordering::Release);
    // Wake compositor to process the next frame.
    let val: u64 = 1;
    unsafe { libc::write(data.wake_fd, &val as *const u64 as *const libc::c_void, 8) };
}

/// Submit an AHardwareBuffer to SurfaceFlinger via ASurfaceTransaction.
/// Sets geometry to scale the buffer to fill the window (dest_w × dest_h).
/// Uses OnComplete callback to signal when the frame is displayed.
pub fn present_buffer(
    surface_control: &SurfaceControl,
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
        let txn = ASurfaceTransaction_create();
        ASurfaceTransaction_setBuffer(txn, surface_control.as_ptr(), buffer.as_ptr(), acquire_fence_fd);
        ASurfaceTransaction_setGeometry(txn, surface_control.as_ptr(), &source, &destination, 0);
        ASurfaceTransaction_setOnComplete(txn, cb_ctx, on_complete_callback);
        ASurfaceTransaction_apply(txn);
        ASurfaceTransaction_delete(txn);
    }
}

/// Set visibility on a surface control (call once after creation).
pub fn set_visible(surface_control: &SurfaceControl) {
    unsafe {
        let txn = ASurfaceTransaction_create();
        ASurfaceTransaction_setVisibility(txn, surface_control.as_ptr(), VISIBILITY_SHOW);
        ASurfaceTransaction_apply(txn);
        ASurfaceTransaction_delete(txn);
    }
}
