//! AHardwareBuffer-backed allocator for server-side dmabuf allocation.
//!
//! Allocates GPU buffers via AHardwareBuffer_allocate and exports them as
//! dmabufs. When the compositor presents a buffer it allocated itself, it can
//! skip the Vulkan staging blit entirely and hand the AHB straight to
//! ASurfaceTransaction — zero GPU copies.
//!
//! The allocator implements smithay's [`Allocator`] trait so it can be used
//! with [`DmabufAllocator`] and [`Swapchain`].

use std::collections::HashMap;
use std::os::unix::io::{OwnedFd, RawFd};
use std::sync::Arc;

use smithay::backend::allocator::dmabuf::{AsDmabuf, Dmabuf, DmabufFlags};
use smithay::backend::allocator::{Allocator, Buffer, Format, Fourcc, Modifier};
use smithay::utils::{Buffer as BufferCoords, Size};

use super::surface_transaction::{
    HardwareBuffer, AHB_FORMAT_R8G8B8A8_UNORM, AHB_USAGE_COMPOSER_OVERLAY,
    AHB_USAGE_GPU_FRAMEBUFFER, AHB_USAGE_GPU_SAMPLED_IMAGE,
};

/// DRM fourcc → AHardwareBuffer format mapping.
fn fourcc_to_ahb_format(fourcc: Fourcc) -> Option<u32> {
    match fourcc {
        // ABGR8888 / XBGR8888 = memory [R,G,B,A] = AHB R8G8B8A8
        Fourcc::Abgr8888 | Fourcc::Xbgr8888 => Some(AHB_FORMAT_R8G8B8A8_UNORM),
        // ARGB8888 / XRGB8888 = memory [B,G,R,A] — no native AHB BGRA format,
        // allocate as R8G8B8A8 (same size/stride, channels reinterpreted).
        // The compositor's blit handles the channel swap if needed.
        Fourcc::Argb8888 | Fourcc::Xrgb8888 => Some(AHB_FORMAT_R8G8B8A8_UNORM),
        _ => None,
    }
}

// ── AhbBuffer ───────────────────────────────────────────────────────────────

/// A buffer backed by an Android AHardwareBuffer.
///
/// Holds a strong reference to the underlying HardwareBuffer (released on drop).
/// Can be exported as a Dmabuf via the [`AsDmabuf`] trait.
#[derive(Debug)]
pub struct AhbBuffer {
    /// The underlying AHardwareBuffer (RAII — released on drop).
    pub ahb: Arc<HardwareBuffer>,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: Format,
}

impl Buffer for AhbBuffer {
    fn size(&self) -> Size<i32, BufferCoords> {
        (self.width as i32, self.height as i32).into()
    }

    fn format(&self) -> Format {
        self.format
    }
}

/// Error exporting an AHB as a dmabuf.
#[derive(Debug)]
pub enum AhbExportError {
    FdExtractionFailed,
    SocketPair(std::io::Error),
    SendHandle(i32),
    RecvHandle(i32),
}

impl std::fmt::Display for AhbExportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FdExtractionFailed => write!(f, "Failed to extract dmabuf fd from AHardwareBuffer"),
            Self::SocketPair(e) => write!(f, "socketpair failed: {e}"),
            Self::SendHandle(r) => write!(f, "AHardwareBuffer_sendHandleToUnixSocket failed: {r}"),
            Self::RecvHandle(r) => write!(f, "AHardwareBuffer_recvHandleFromUnixSocket failed: {r}"),
        }
    }
}

impl std::error::Error for AhbExportError {}

impl AsDmabuf for AhbBuffer {
    type Error = AhbExportError;

    fn export(&self) -> Result<Dmabuf, Self::Error> {
        // Extract the dmabuf fd from the AHardwareBuffer.
        // Android doesn't have a direct "get dmabuf fd" API, but we can use
        // the Unix socket handle-passing mechanism: create a socketpair,
        // send the AHB handle over one end, receive a duplicate on the other.
        //
        // This gives us an AHardwareBuffer on the receiving side whose
        // underlying gralloc buffer is the same physical memory, and we can
        // then use a simple ioctl or /proc approach. However, the simplest
        // portable approach on Android 10+ is to use the NDK
        // AHardwareBuffer_sendHandleToUnixSocket / recv pair and then
        // extract the fd using the lower-level gralloc HAL.
        //
        // For our use case, we take a different approach: since the AHB is
        // already allocated with GPU_FRAMEBUFFER | COMPOSER_OVERLAY usage,
        // we use the Vulkan AHB import path to get the dmabuf fd. But we
        // can also directly use the Android native_handle_t which contains
        // the dmabuf fd(s).
        //
        // On Qualcomm devices (cros_gralloc / msm), the first fd in the
        // native_handle is the dmabuf fd. We access this via
        // AHardwareBuffer_getNativeHandle (available in the NDK as a
        // system API, or via dlsym).
        let fd = get_ahb_dmabuf_fd(self.ahb.as_ptr())?;

        // AHB is always single-plane for RGBA formats.
        let mut builder = Dmabuf::builder(
            (self.width as i32, self.height as i32),
            self.format.code,
            self.format.modifier,
            DmabufFlags::empty(),
        );
        builder.add_plane(fd, 0, 0, self.stride);

        builder
            .build()
            .ok_or(AhbExportError::FdExtractionFailed)
    }
}

/// Extract the dmabuf file descriptor from an AHardwareBuffer.
///
/// On Android, AHardwareBuffer wraps a gralloc buffer whose native_handle_t
/// contains dmabuf fds. We access this via the NDK's
/// `AHardwareBuffer_getNativeHandle` (added in API 26, but not in the public
/// NDK headers until API 30). We use dlsym to get it.
///
/// The returned OwnedFd is a dup of the original — caller owns it.
fn get_ahb_dmabuf_fd(
    ahb: *mut ndk_sys::AHardwareBuffer,
) -> Result<OwnedFd, AhbExportError> {
    // native_handle_t layout (from <cutils/native_handle.h>):
    //   int version;    // = sizeof(native_handle_t)
    //   int numFds;     // number of file descriptors
    //   int numInts;    // number of ints
    //   int data[];     // fds first, then ints
    //
    // The first fd is the dmabuf fd for single-plane formats on all
    // Android gralloc implementations (Qualcomm, ARM, Intel, cros_gralloc).

    type GetNativeHandleFn =
        unsafe extern "C" fn(*const ndk_sys::AHardwareBuffer) -> *const NativeHandle;

    #[repr(C)]
    struct NativeHandle {
        version: i32,
        num_fds: i32,
        num_ints: i32,
        // data[] follows: first num_fds file descriptors, then num_ints ints
    }

    // Try to load AHardwareBuffer_getNativeHandle via dlsym.
    // This is a system API available on all Android 8+ devices.
    static GET_NATIVE_HANDLE: std::sync::OnceLock<Option<GetNativeHandleFn>> =
        std::sync::OnceLock::new();

    let func = GET_NATIVE_HANDLE.get_or_init(|| {
        let lib = unsafe { libc::dlopen(c"libandroid.so".as_ptr(), libc::RTLD_NOW) };
        if lib.is_null() {
            tracing::warn!("Failed to dlopen libandroid.so");
            return None;
        }
        let sym = unsafe {
            libc::dlsym(
                lib,
                c"AHardwareBuffer_getNativeHandle".as_ptr(),
            )
        };
        if sym.is_null() {
            tracing::warn!("AHardwareBuffer_getNativeHandle not found in libandroid.so");
            None
        } else {
            Some(unsafe { std::mem::transmute::<*mut libc::c_void, GetNativeHandleFn>(sym) })
        }
    });

    let get_native_handle = func.ok_or(AhbExportError::FdExtractionFailed)?;

    let handle = unsafe { get_native_handle(ahb) };
    if handle.is_null() {
        return Err(AhbExportError::FdExtractionFailed);
    }

    let native_handle = unsafe { &*handle };
    if native_handle.num_fds < 1 {
        tracing::error!(
            "native_handle has no fds (num_fds={})",
            native_handle.num_fds
        );
        return Err(AhbExportError::FdExtractionFailed);
    }

    // The data array starts right after the struct fields.
    let data_ptr = unsafe { (handle as *const i32).add(3) };
    let dmabuf_fd_raw = unsafe { *data_ptr };

    // Dup the fd so we own it independently of the AHB's lifetime.
    let duped = unsafe { libc::dup(dmabuf_fd_raw) };
    if duped < 0 {
        return Err(AhbExportError::FdExtractionFailed);
    }

    Ok(unsafe { OwnedFd::from_raw_fd(duped) })
}

use std::os::unix::io::FromRawFd;

// ── AhbAllocator ────────────────────────────────────────────────────────────

/// Allocator that creates AHardwareBuffer-backed buffers.
///
/// Wraps `AHardwareBuffer_allocate` and tracks allocated buffers so the
/// compositor can recognize its own buffers at commit time.
pub struct AhbAllocator {
    /// Usage flags for allocated buffers.
    usage: u64,
}

impl std::fmt::Debug for AhbAllocator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AhbAllocator")
            .field("usage", &self.usage)
            .finish()
    }
}

/// Error type for AHB allocation failures.
#[derive(Debug)]
pub enum AhbAllocError {
    UnsupportedFormat(Fourcc),
    AllocationFailed(u32, u32),
}

impl std::fmt::Display for AhbAllocError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedFormat(fourcc) => write!(f, "Unsupported format: {fourcc:?}"),
            Self::AllocationFailed(w, h) => write!(f, "AHardwareBuffer_allocate failed for {w}x{h}"),
        }
    }
}

impl std::error::Error for AhbAllocError {}

impl Default for AhbAllocator {
    fn default() -> Self {
        Self::new()
    }
}

impl AhbAllocator {
    /// Create a new AHB allocator with default Android compositor usage flags.
    #[must_use]
    pub fn new() -> Self {
        Self {
            usage: AHB_USAGE_GPU_FRAMEBUFFER
                | AHB_USAGE_GPU_SAMPLED_IMAGE
                | AHB_USAGE_COMPOSER_OVERLAY,
        }
    }
}

impl Allocator for AhbAllocator {
    type Buffer = AhbBuffer;
    type Error = AhbAllocError;

    fn create_buffer(
        &mut self,
        width: u32,
        height: u32,
        fourcc: Fourcc,
        _modifiers: &[Modifier],
    ) -> Result<Self::Buffer, Self::Error> {
        let ahb_format =
            fourcc_to_ahb_format(fourcc).ok_or(AhbAllocError::UnsupportedFormat(fourcc))?;

        let ahb = HardwareBuffer::allocate(width, height, ahb_format, self.usage)
            .ok_or(AhbAllocError::AllocationFailed(width, height))?;

        // Query the actual stride from the allocated buffer.
        let mut desc = ndk_sys::AHardwareBuffer_Desc {
            width: 0,
            height: 0,
            layers: 0,
            format: 0,
            usage: 0,
            stride: 0,
            rfu0: 0,
            rfu1: 0,
        };
        unsafe { ndk_sys::AHardwareBuffer_describe(ahb.as_ptr(), &mut desc) };

        // stride is in pixels; convert to bytes (4 bytes per pixel for RGBA).
        let stride_bytes = desc.stride * 4;

        tracing::info!(
            "[ahb-alloc] Allocated {}x{} fmt={:?} stride={}px ({}B)",
            width,
            height,
            fourcc,
            desc.stride,
            stride_bytes,
        );

        Ok(AhbBuffer {
            ahb: Arc::new(ahb),
            width,
            height,
            stride: stride_bytes,
            format: Format {
                code: fourcc,
                // AHB doesn't expose DRM modifiers; treat as implicit/vendor-specific.
                modifier: Modifier::Invalid,
            },
        })
    }
}

// ── AhbBufferTracker ────────────────────────────────────────────────────────

/// Unique identity of a DMA buffer, derived from `fstat()` on the dmabuf fd.
/// Two fds that refer to the same DMA buffer will have the same (dev, ino).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct DmabufInode {
    dev: u64,
    ino: u64,
}

/// Get the (dev, ino) for a file descriptor, used to identify DMA buffers
/// across different fd numbers (dup'd fds, or fds passed via Unix socket).
fn fd_inode(fd: RawFd) -> Option<DmabufInode> {
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    let ret = unsafe { libc::fstat(fd, &mut stat) };
    if ret == 0 {
        Some(DmabufInode {
            dev: stat.st_dev,
            ino: stat.st_ino,
        })
    } else {
        None
    }
}

/// Tracks compositor-allocated AHB buffers so we can recognize them when
/// a client commits a frame rendered into one.
///
/// Uses the DMA buffer's inode (from fstat) as the key, which allows matching
/// across different fd numbers — the client will have a dup'd fd that points
/// to the same underlying kernel DMA buffer.
#[derive(Debug)]
pub struct AhbBufferTracker {
    /// Map from DMA buffer inode to the compositor-allocated AhbBuffer.
    buffers: HashMap<DmabufInode, TrackedAhb>,
}

#[derive(Debug)]
struct TrackedAhb {
    /// The compositor-allocated buffer.
    pub buffer: AhbBuffer,
    /// The dmabuf exported from this buffer (kept alive so the fd stays valid).
    pub dmabuf: Dmabuf,
}

impl Default for AhbBufferTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl AhbBufferTracker {
    #[must_use]
    pub fn new() -> Self {
        Self {
            buffers: HashMap::new(),
        }
    }

    /// Register a compositor-allocated buffer. Returns the exported Dmabuf.
    pub fn track(&mut self, buffer: AhbBuffer) -> Result<Dmabuf, AhbExportError> {
        let dmabuf = buffer.export()?;

        // Get the inode of the first plane fd.
        let inode = dmabuf
            .handles()
            .next()
            .and_then(|fd| {
                use std::os::unix::io::AsRawFd;
                fd_inode(fd.as_raw_fd())
            })
            .ok_or(AhbExportError::FdExtractionFailed)?;

        tracing::info!(
            "[ahb-tracker] Tracking AHB {}x{} inode=({}, {})",
            buffer.width,
            buffer.height,
            inode.dev,
            inode.ino,
        );
        self.buffers.insert(
            inode,
            TrackedAhb {
                buffer,
                dmabuf: dmabuf.clone(),
            },
        );
        Ok(dmabuf)
    }

    /// Look up whether a committed dmabuf is one of our compositor-allocated AHBs.
    /// Compares by DMA buffer inode so it works even when the client has a
    /// different fd number for the same underlying buffer.
    /// Returns a reference to the AhbBuffer if found.
    pub fn lookup(&self, dmabuf: &Dmabuf) -> Option<&AhbBuffer> {
        let inode = dmabuf.handles().next().and_then(|fd| {
            use std::os::unix::io::AsRawFd;
            fd_inode(fd.as_raw_fd())
        })?;
        self.buffers.get(&inode).map(|t| &t.buffer)
    }

    /// Look up by dmabuf and also return the exported Dmabuf (for buffer release).
    pub fn lookup_with_dmabuf(&self, dmabuf: &Dmabuf) -> Option<(&AhbBuffer, &Dmabuf)> {
        let inode = dmabuf.handles().next().and_then(|fd| {
            use std::os::unix::io::AsRawFd;
            fd_inode(fd.as_raw_fd())
        })?;
        self.buffers.get(&inode).map(|t| (&t.buffer, &t.dmabuf))
    }

    /// Remove a tracked buffer by its dmabuf.
    pub fn untrack(&mut self, dmabuf: &Dmabuf) {
        if let Some(inode) = dmabuf.handles().next().and_then(|fd| {
            use std::os::unix::io::AsRawFd;
            fd_inode(fd.as_raw_fd())
        }) {
            self.buffers.remove(&inode);
        }
    }

    /// Remove all tracked buffers.
    pub fn clear(&mut self) {
        self.buffers.clear();
    }

    /// Number of tracked buffers.
    pub fn len(&self) -> usize {
        self.buffers.len()
    }

    /// Returns true if no buffers are tracked.
    pub fn is_empty(&self) -> bool {
        self.buffers.is_empty()
    }
}

// ── Per-window AHB pool ─────────────────────────────────────────────────────

/// A pool of compositor-allocated AHBs for a single window.
///
/// Triple-buffered: one buffer is being displayed (in SurfaceFlinger),
/// one is being rendered into by the client, one is free for the next frame.
pub struct AhbWindowPool {
    /// Available buffers (not currently in use by client or SurfaceFlinger).
    free: Vec<AhbBuffer>,
    /// Buffer dimensions.
    pub width: u32,
    pub height: u32,
    pub format: Fourcc,
}

impl std::fmt::Debug for AhbWindowPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AhbWindowPool")
            .field("free_count", &self.free.len())
            .field("width", &self.width)
            .field("height", &self.height)
            .field("format", &self.format)
            .finish()
    }
}

impl AhbWindowPool {
    /// Create a new pool and pre-allocate buffers.
    pub fn new(
        allocator: &mut AhbAllocator,
        width: u32,
        height: u32,
        format: Fourcc,
        count: usize,
    ) -> Result<Self, AhbAllocError> {
        let mut free = Vec::with_capacity(count);
        for _ in 0..count {
            free.push(allocator.create_buffer(
                width,
                height,
                format,
                &[Modifier::Invalid],
            )?);
        }
        tracing::info!(
            "[ahb-pool] Created pool: {}x{} fmt={:?} count={}",
            width,
            height,
            format,
            count,
        );
        Ok(Self {
            free,
            width,
            height,
            format,
        })
    }

    /// Take a free buffer from the pool. Returns None if all buffers are in use.
    pub fn acquire(&mut self) -> Option<AhbBuffer> {
        self.free.pop()
    }

    /// Return a buffer to the pool after the client is done with it.
    pub fn release(&mut self, buffer: AhbBuffer) {
        self.free.push(buffer);
    }

    /// Check if the pool dimensions match.
    pub fn matches(&self, width: u32, height: u32) -> bool {
        self.width == width && self.height == height
    }
}
