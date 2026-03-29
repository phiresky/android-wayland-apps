//! DRM render node shim for Firefox dmabuf support in proot.
//!
//! Firefox's nsDMABufDevice opens /dev/dri/renderD128 and calls
//! drmGetVersion() to verify it's a real DRM device. On Android,
//! SELinux blocks app access to DRM nodes. We bind /dev/null as
//! renderD128 so open() succeeds, but ioctl(DRM_IOCTL_VERSION)
//! fails with ENOTTY.
//!
//! This shim intercepts ioctl() on fds opened from /dev/dri/ and
//! fakes DRM version/capability responses. All other calls pass through.
//!
//! Build (inside proot):
//!   rustc --edition 2021 --crate-type cdylib -o /usr/local/lib/drm_shim.so drm_shim.rs
//!
//! Usage:
//!   LD_PRELOAD=/usr/local/lib/drm_shim.so firefox

use std::ffi::{c_char, c_int, c_ulong, c_void, CStr};
use std::sync::Mutex;

// ── libc / dlsym bindings ──────────────────────────────────────────────────

extern "C" {
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

const RTLD_NEXT: *mut c_void = -1isize as *mut c_void;

unsafe fn get_real<F>(name: &CStr) -> F {
    let ptr = unsafe { dlsym(RTLD_NEXT, name.as_ptr()) };
    assert!(!ptr.is_null(), "dlsym({:?}) returned null", name);
    unsafe { std::mem::transmute_copy(&ptr) }
}

// ── DRM ioctl constants ───────────────────────────────────────────────────

const DRM_IOCTL_BASE: u32 = b'd' as u32;

// _IOWR('d', 0x00, drm_version) — size varies by arch, compute it
const DRM_VERSION_SIZE: u32 = std::mem::size_of::<DrmVersion>() as u32;
const DRM_IOCTL_VERSION: c_ulong =
    (3 << 30) | ((DRM_IOCTL_BASE as c_ulong) << 8) | 0x00 | ((DRM_VERSION_SIZE as c_ulong) << 16);

const DRM_GET_CAP_SIZE: u32 = std::mem::size_of::<DrmGetCap>() as u32;
const DRM_IOCTL_GET_CAP: c_ulong =
    (3 << 30) | ((DRM_IOCTL_BASE as c_ulong) << 8) | 0x0c | ((DRM_GET_CAP_SIZE as c_ulong) << 16);

const DRM_CAP_PRIME: u64 = 0x05;
const DRM_CAP_ADDFB2_MODIFIERS: u64 = 0x10;

#[repr(C)]
struct DrmVersion {
    version_major: c_int,
    version_minor: c_int,
    version_patchlevel: c_int,
    name_len: usize,
    name: *mut c_char,
    date_len: usize,
    date: *mut c_char,
    desc_len: usize,
    desc: *mut c_char,
}

#[repr(C)]
struct DrmGetCap {
    capability: u64,
    value: u64,
}

// ── Tracked fds ───────────────────────────────────────────────────────────

static SHIM_FDS: Mutex<Vec<c_int>> = Mutex::new(Vec::new());

fn is_shim_fd(fd: c_int) -> bool {
    SHIM_FDS.lock().map(|fds| fds.contains(&fd)).unwrap_or(false)
}

fn track_fd(fd: c_int) {
    if let Ok(mut fds) = SHIM_FDS.lock() {
        if !fds.contains(&fd) {
            fds.push(fd);
        }
    }
}

fn untrack_fd(fd: c_int) {
    if let Ok(mut fds) = SHIM_FDS.lock() {
        fds.retain(|&f| f != fd);
    }
}

fn fill_str(dst: *mut c_char, len: &mut usize, src: &[u8]) {
    if !dst.is_null() && *len > 0 {
        let copy = src.len().min(*len);
        unsafe { std::ptr::copy_nonoverlapping(src.as_ptr(), dst as *mut u8, copy) };
    }
    *len = src.len();
}

// ── Intercepted functions ─────────────────────────────────────────────────

type OpenFn = unsafe extern "C" fn(*const c_char, c_int, ...) -> c_int;
type CloseFn = unsafe extern "C" fn(c_int) -> c_int;
type IoctlFn = unsafe extern "C" fn(c_int, c_ulong, ...) -> c_int;

/// Intercept open() to track fds from /dev/dri/
#[unsafe(no_mangle)]
pub unsafe extern "C" fn open(path: *const c_char, flags: c_int, args: ...) -> c_int {
    let real_open: OpenFn = unsafe { get_real(c"open") };
    // O_CREAT=0o100, O_TMPFILE=0o20200000 on aarch64-linux
    let mode: u32 = if flags & (0o100 | 0o20200000) != 0 {
        args.arg::<u32>()
    } else {
        0
    };

    let fd = unsafe { real_open(path, flags, mode) };

    if fd >= 0 && !path.is_null() {
        let p = unsafe { CStr::from_ptr(path) };
        if let Ok(s) = p.to_str() {
            if s.starts_with("/dev/dri/") {
                track_fd(fd);
                eprintln!("[drm-shim] Tracking fd={fd} for {s}");
            }
        }
    }

    fd
}

/// Intercept open64() — same as open() on 64-bit
#[unsafe(no_mangle)]
pub unsafe extern "C" fn open64(path: *const c_char, flags: c_int, args: ...) -> c_int {
    let real_open64: OpenFn = unsafe { get_real(c"open64") };
    let mode: u32 = if flags & (0o100 | 0o20200000) != 0 {
        args.arg::<u32>()
    } else {
        0
    };

    let fd = unsafe { real_open64(path, flags, mode) };

    if fd >= 0 && !path.is_null() {
        let p = unsafe { CStr::from_ptr(path) };
        if let Ok(s) = p.to_str() {
            if s.starts_with("/dev/dri/") {
                track_fd(fd);
                eprintln!("[drm-shim] Tracking fd={fd} for {s} (open64)");
            }
        }
    }

    fd
}

/// Intercept close() to untrack fds
#[unsafe(no_mangle)]
pub unsafe extern "C" fn close(fd: c_int) -> c_int {
    let real_close: CloseFn = unsafe { get_real(c"close") };
    untrack_fd(fd);
    unsafe { real_close(fd) }
}

/// Intercept ioctl() to fake DRM responses on tracked fds
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ioctl(fd: c_int, request: c_ulong, args: ...) -> c_int {
    let real_ioctl: IoctlFn = unsafe { get_real(c"ioctl") };
    let arg: *mut c_void = args.arg::<*mut c_void>();

    if !is_shim_fd(fd) {
        return unsafe { real_ioctl(fd, request, arg) };
    }

    // Handle DRM ioctls on our fake render node
    if request == DRM_IOCTL_VERSION {
        let v = unsafe { &mut *(arg as *mut DrmVersion) };
        v.version_major = 1;
        v.version_minor = 0;
        v.version_patchlevel = 0;
        fill_str(v.name, &mut v.name_len, b"drm-shim");
        fill_str(v.date, &mut v.date_len, b"20260329");
        fill_str(v.desc, &mut v.desc_len, b"Fake DRM for Firefox dmabuf");
        return 0;
    }

    if request == DRM_IOCTL_GET_CAP {
        let cap = unsafe { &mut *(arg as *mut DrmGetCap) };
        cap.value = match cap.capability {
            DRM_CAP_PRIME => 3, // IMPORT | EXPORT
            DRM_CAP_ADDFB2_MODIFIERS => 1,
            _ => 0,
        };
        return 0;
    }

    // All other DRM ioctls: return success
    0
}
