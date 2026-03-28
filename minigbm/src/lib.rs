//! GBM (Generic Buffer Manager) for Android, backed by AHardwareBuffer.
//!
//! Provides safe Rust wrappers around the GBM C API. Buffers are allocated
//! via Android's AHardwareBuffer, with dmabuf fds extracted from the native
//! handle for sharing with Linux clients in proot.
//!
//! # Usage
//!
//! ```no_run
//! use minigbm::{GbmDevice, GbmFormat, GbmBoFlags};
//!
//! let device = GbmDevice::new(-1).expect("failed to create device");
//! let bo = device
//!     .create_bo(1920, 1080, GbmFormat::ABGR8888, GbmBoFlags::RENDERING | GbmBoFlags::SCANOUT)
//!     .expect("failed to create buffer");
//!
//! let fd = bo.get_fd().expect("failed to get dmabuf fd");
//! let stride = bo.stride();
//! ```

#![allow(non_camel_case_types)]

use std::os::unix::io::RawFd;

// ── Raw FFI bindings ─────────────────────────────────────────────────────

#[repr(C)]
pub struct gbm_device {
    _opaque: [u8; 0],
}

#[repr(C)]
pub struct gbm_bo {
    _opaque: [u8; 0],
}

#[repr(C)]
pub struct gbm_surface {
    _opaque: [u8; 0],
}

#[repr(C)]
#[derive(Copy, Clone)]
pub union gbm_bo_handle {
    pub ptr: *mut std::ffi::c_void,
    pub s32: i32,
    pub u32_: u32,
    pub s64: i64,
    pub u64_: u64,
}

unsafe extern "C" {
    // Device
    pub fn gbm_create_device(fd: i32) -> *mut gbm_device;
    pub fn gbm_device_destroy(dev: *mut gbm_device);
    pub fn gbm_device_get_fd(dev: *mut gbm_device) -> i32;
    pub fn gbm_device_get_backend_name(dev: *mut gbm_device) -> *const std::ffi::c_char;
    pub fn gbm_device_is_format_supported(dev: *mut gbm_device, format: u32, usage: u32) -> i32;

    // Buffer object
    pub fn gbm_bo_create(
        dev: *mut gbm_device,
        width: u32,
        height: u32,
        format: u32,
        flags: u32,
    ) -> *mut gbm_bo;
    pub fn gbm_bo_create_with_modifiers(
        dev: *mut gbm_device,
        width: u32,
        height: u32,
        format: u32,
        modifiers: *const u64,
        count: u32,
    ) -> *mut gbm_bo;
    pub fn gbm_bo_destroy(bo: *mut gbm_bo);

    pub fn gbm_bo_get_width(bo: *mut gbm_bo) -> u32;
    pub fn gbm_bo_get_height(bo: *mut gbm_bo) -> u32;
    pub fn gbm_bo_get_stride(bo: *mut gbm_bo) -> u32;
    pub fn gbm_bo_get_stride_for_plane(bo: *mut gbm_bo, plane: i32) -> u32;
    pub fn gbm_bo_get_format(bo: *mut gbm_bo) -> u32;
    pub fn gbm_bo_get_bpp(bo: *mut gbm_bo) -> u32;
    pub fn gbm_bo_get_modifier(bo: *mut gbm_bo) -> u64;
    pub fn gbm_bo_get_handle(bo: *mut gbm_bo) -> gbm_bo_handle;
    pub fn gbm_bo_get_fd(bo: *mut gbm_bo) -> i32;
    pub fn gbm_bo_get_fd_for_plane(bo: *mut gbm_bo, plane: i32) -> i32;
    pub fn gbm_bo_get_plane_count(bo: *mut gbm_bo) -> i32;
    pub fn gbm_bo_get_offset(bo: *mut gbm_bo, plane: i32) -> u32;
    pub fn gbm_bo_get_plane_size(bo: *mut gbm_bo, plane: usize) -> u32;

    pub fn gbm_bo_map(
        bo: *mut gbm_bo,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        flags: u32,
        stride: *mut u32,
        map_data: *mut *mut std::ffi::c_void,
    ) -> *mut std::ffi::c_void;
    pub fn gbm_bo_unmap(bo: *mut gbm_bo, map_data: *mut std::ffi::c_void);

    pub fn gbm_bo_set_user_data(
        bo: *mut gbm_bo,
        data: *mut std::ffi::c_void,
        destroy: Option<unsafe extern "C" fn(*mut gbm_bo, *mut std::ffi::c_void)>,
    );
    pub fn gbm_bo_get_user_data(bo: *mut gbm_bo) -> *mut std::ffi::c_void;

    // Android extension: get underlying AHardwareBuffer
    pub fn gbm_bo_get_ahardwarebuffer(bo: *mut gbm_bo) -> *mut std::ffi::c_void;
}

// ── DRM fourcc format constants ──────────────────────────────────────────

/// DRM fourcc format codes, compatible with GBM_FORMAT_* and DRM_FORMAT_*.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum GbmFormat {
    XRGB8888 = fourcc(b'X', b'R', b'2', b'4'),
    XBGR8888 = fourcc(b'X', b'B', b'2', b'4'),
    ARGB8888 = fourcc(b'A', b'R', b'2', b'4'),
    ABGR8888 = fourcc(b'A', b'B', b'2', b'4'),
    RGBX8888 = fourcc(b'R', b'X', b'2', b'4'),
    BGRX8888 = fourcc(b'B', b'X', b'2', b'4'),
    RGBA8888 = fourcc(b'R', b'A', b'2', b'4'),
    BGRA8888 = fourcc(b'B', b'A', b'2', b'4'),
    RGB565 = fourcc(b'R', b'G', b'1', b'6'),
    RGB888 = fourcc(b'R', b'G', b'2', b'4'),
    ABGR2101010 = fourcc(b'A', b'B', b'3', b'0'),
    ABGR16161616F = fourcc(b'A', b'B', b'4', b'H'),
}

const fn fourcc(a: u8, b: u8, c: u8, d: u8) -> u32 {
    (a as u32) | ((b as u32) << 8) | ((c as u32) << 16) | ((d as u32) << 24)
}

impl GbmFormat {
    pub fn as_u32(self) -> u32 {
        self as u32
    }
}

// ── GBM BO flags ─────────────────────────────────────────────────────────

/// Minimal bitflags implementation (avoids external dependency).
macro_rules! bitflags_manual {
    (
        $(#[$outer:meta])*
        pub struct $Name:ident : $T:ty {
            $(const $Flag:ident = $value:expr;)*
        }
    ) => {
        $(#[$outer])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub struct $Name($T);

        impl $Name {
            $(pub const $Flag: Self = Self($value);)*

            pub fn bits(self) -> $T { self.0 }
            pub fn from_bits_truncate(bits: $T) -> Self { Self(bits) }
            pub fn contains(self, other: Self) -> bool { (self.0 & other.0) == other.0 }
        }

        impl std::ops::BitOr for $Name {
            type Output = Self;
            fn bitor(self, rhs: Self) -> Self { Self(self.0 | rhs.0) }
        }

        impl std::ops::BitOrAssign for $Name {
            fn bitor_assign(&mut self, rhs: Self) { self.0 |= rhs.0; }
        }

        impl std::ops::BitAnd for $Name {
            type Output = Self;
            fn bitand(self, rhs: Self) -> Self { Self(self.0 & rhs.0) }
        }
    };
}

bitflags_manual! {
    /// Buffer usage flags for `gbm_bo_create`.
    pub struct GbmBoFlags: u32 {
        const SCANOUT        = 1 << 0;
        const CURSOR         = 1 << 1;
        const RENDERING      = 1 << 2;
        const WRITE          = 1 << 3;
        const LINEAR         = 1 << 4;
        const TEXTURING      = 1 << 5;
        const SW_READ_OFTEN  = 1 << 9;
        const SW_READ_RARELY = 1 << 10;
        const SW_WRITE_OFTEN = 1 << 11;
        const SW_WRITE_RARELY = 1 << 12;
    }
}

// ── Safe wrappers ────────────────────────────────────────────────────────

/// A GBM device backed by Android AHardwareBuffer.
///
/// The `fd` parameter is stored but not used for allocation (AHardwareBuffer
/// allocates via Android's gralloc HAL). Pass -1 or a render node fd.
pub struct GbmDevice {
    raw: *mut gbm_device,
}

// GBM device is thread-safe (no mutable shared state in our implementation)
unsafe impl Send for GbmDevice {}
unsafe impl Sync for GbmDevice {}

impl GbmDevice {
    /// Create a new GBM device.
    ///
    /// `fd` is stored but not used for buffer allocation. Pass -1 if you
    /// don't have a DRM render node. On Android, buffers are allocated
    /// via AHardwareBuffer regardless of the fd.
    pub fn new(fd: RawFd) -> Option<Self> {
        let raw = unsafe { gbm_create_device(fd) };
        if raw.is_null() {
            None
        } else {
            Some(Self { raw })
        }
    }

    /// Get the file descriptor associated with this device.
    pub fn fd(&self) -> RawFd {
        unsafe { gbm_device_get_fd(self.raw) }
    }

    /// Get the backend name (always "ahardwarebuffer" for this implementation).
    pub fn backend_name(&self) -> &str {
        let ptr = unsafe { gbm_device_get_backend_name(self.raw) };
        if ptr.is_null() {
            "unknown"
        } else {
            unsafe { std::ffi::CStr::from_ptr(ptr) }
                .to_str()
                .unwrap_or("unknown")
        }
    }

    /// Check if a format+usage combination is supported.
    pub fn is_format_supported(&self, format: GbmFormat, usage: GbmBoFlags) -> bool {
        unsafe { gbm_device_is_format_supported(self.raw, format.as_u32(), usage.bits()) != 0 }
    }

    /// Allocate a buffer object.
    pub fn create_bo(
        &self,
        width: u32,
        height: u32,
        format: GbmFormat,
        flags: GbmBoFlags,
    ) -> Option<GbmBo> {
        let bo = unsafe { gbm_bo_create(self.raw, width, height, format.as_u32(), flags.bits()) };
        if bo.is_null() {
            None
        } else {
            Some(GbmBo { raw: bo })
        }
    }

    /// Allocate a buffer object with a raw u32 format code (for formats not in the enum).
    pub fn create_bo_raw(
        &self,
        width: u32,
        height: u32,
        format: u32,
        flags: u32,
    ) -> Option<GbmBo> {
        let bo = unsafe { gbm_bo_create(self.raw, width, height, format, flags) };
        if bo.is_null() {
            None
        } else {
            Some(GbmBo { raw: bo })
        }
    }

    /// Get the raw C pointer (for interop with other C/FFI code).
    pub fn as_raw(&self) -> *mut gbm_device {
        self.raw
    }
}

impl Drop for GbmDevice {
    fn drop(&mut self) {
        unsafe { gbm_device_destroy(self.raw) };
    }
}

/// A GBM buffer object backed by an AHardwareBuffer.
pub struct GbmBo {
    raw: *mut gbm_bo,
}

unsafe impl Send for GbmBo {}

impl GbmBo {
    /// Buffer width in pixels.
    pub fn width(&self) -> u32 {
        unsafe { gbm_bo_get_width(self.raw) }
    }

    /// Buffer height in pixels.
    pub fn height(&self) -> u32 {
        unsafe { gbm_bo_get_height(self.raw) }
    }

    /// Buffer stride in bytes (for plane 0).
    pub fn stride(&self) -> u32 {
        unsafe { gbm_bo_get_stride(self.raw) }
    }

    /// Buffer format as DRM fourcc code.
    pub fn format(&self) -> u32 {
        unsafe { gbm_bo_get_format(self.raw) }
    }

    /// Bytes per pixel.
    pub fn bpp(&self) -> u32 {
        unsafe { gbm_bo_get_bpp(self.raw) }
    }

    /// Format modifier (always LINEAR for AHardwareBuffer backend).
    pub fn modifier(&self) -> u64 {
        unsafe { gbm_bo_get_modifier(self.raw) }
    }

    /// Number of planes (always 1 for supported RGB formats).
    pub fn plane_count(&self) -> i32 {
        unsafe { gbm_bo_get_plane_count(self.raw) }
    }

    /// Get a dmabuf file descriptor for this buffer.
    ///
    /// Returns a new dup'd fd on each call (caller must close it).
    /// Returns `None` if dmabuf extraction fails.
    pub fn get_fd(&self) -> Option<RawFd> {
        let fd = unsafe { gbm_bo_get_fd(self.raw) };
        if fd < 0 { None } else { Some(fd) }
    }

    /// Get the underlying AHardwareBuffer pointer.
    ///
    /// This is an Android-specific extension. The returned pointer can be
    /// used with Vulkan's `VK_ANDROID_external_memory_android_hardware_buffer`
    /// or `ASurfaceTransaction_setBuffer`.
    ///
    /// The pointer is valid for the lifetime of this `GbmBo`.
    pub fn ahardwarebuffer_ptr(&self) -> *mut std::ffi::c_void {
        unsafe { gbm_bo_get_ahardwarebuffer(self.raw) }
    }

    /// Get the raw C pointer (for interop).
    pub fn as_raw(&self) -> *mut gbm_bo {
        self.raw
    }
}

impl Drop for GbmBo {
    fn drop(&mut self) {
        unsafe { gbm_bo_destroy(self.raw) };
    }
}

impl std::fmt::Debug for GbmBo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let fmt_code = self.format();
        let fmt_chars = [
            (fmt_code & 0xFF) as u8 as char,
            ((fmt_code >> 8) & 0xFF) as u8 as char,
            ((fmt_code >> 16) & 0xFF) as u8 as char,
            ((fmt_code >> 24) & 0xFF) as u8 as char,
        ];
        f.debug_struct("GbmBo")
            .field("width", &self.width())
            .field("height", &self.height())
            .field("stride", &self.stride())
            .field("format", &format_args!("{}", fmt_chars.iter().collect::<String>()))
            .field("planes", &self.plane_count())
            .finish()
    }
}

// ── On-device smoke test ─────────────────────────────────────────────────

/// Run a smoke test of the GBM API on Android.
///
/// Creates a device, allocates a buffer, extracts a dmabuf fd, and verifies
/// all properties. Returns `Ok(summary)` on success or `Err(msg)` on failure.
///
/// This is meant to be called from the Android app at startup for verification.
pub fn smoke_test() -> Result<String, String> {
    let device = GbmDevice::new(-1).ok_or("failed to create GBM device")?;

    let backend = device.backend_name();
    if backend != "ahardwarebuffer" {
        return Err(format!("unexpected backend: {backend}"));
    }

    if !device.is_format_supported(GbmFormat::ABGR8888, GbmBoFlags::RENDERING) {
        return Err("ABGR8888 not supported".into());
    }

    let bo = device
        .create_bo(
            64,
            64,
            GbmFormat::ABGR8888,
            GbmBoFlags::RENDERING | GbmBoFlags::SCANOUT,
        )
        .ok_or("failed to create 64x64 ABGR8888 buffer")?;

    if bo.width() != 64 || bo.height() != 64 {
        return Err(format!(
            "wrong dimensions: {}x{} (expected 64x64)",
            bo.width(),
            bo.height()
        ));
    }

    if bo.stride() == 0 {
        return Err("stride is 0".into());
    }

    if bo.bpp() != 4 {
        return Err(format!("wrong bpp: {} (expected 4)", bo.bpp()));
    }

    if bo.plane_count() != 1 {
        return Err(format!(
            "wrong plane count: {} (expected 1)",
            bo.plane_count()
        ));
    }

    let fd = bo.get_fd().ok_or("failed to get dmabuf fd")?;
    if fd < 0 {
        return Err(format!("invalid dmabuf fd: {fd}"));
    }

    // Verify fd is valid by checking it with fstat
    let valid = unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0;
    unsafe { libc::close(fd) };

    if !valid {
        return Err("dmabuf fd is not valid (fcntl F_GETFD failed)".into());
    }

    let ahb_ptr = bo.ahardwarebuffer_ptr();
    if ahb_ptr.is_null() {
        return Err("AHardwareBuffer pointer is null".into());
    }

    Ok(format!(
        "GBM smoke test passed: backend={backend}, bo={bo:?}, \
         dmabuf_fd={fd}, ahb={ahb_ptr:?}"
    ))
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_fourcc_values() {
        // Verify our fourcc constants match expected DRM values
        assert_eq!(GbmFormat::ABGR8888.as_u32(), 0x34324241);
        assert_eq!(GbmFormat::XRGB8888.as_u32(), 0x34325258);
        assert_eq!(GbmFormat::RGB565.as_u32(), 0x36314752);
    }

    #[test]
    fn test_flags_bitops() {
        let flags = GbmBoFlags::RENDERING | GbmBoFlags::SCANOUT;
        assert!(flags.contains(GbmBoFlags::RENDERING));
        assert!(flags.contains(GbmBoFlags::SCANOUT));
        assert!(!flags.contains(GbmBoFlags::LINEAR));
        assert_eq!(flags.bits(), 0b101);
    }
}
