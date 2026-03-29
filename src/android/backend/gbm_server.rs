//! GBM allocator server: listens on a Unix socket in the proot rootfs
//! and allocates AHardwareBuffers on behalf of proot clients.
//!
//! Protocol:
//! - Client connects to `{ARCH_FS_ROOT}/tmp/gbm-alloc-0`
//! - Client sends `AllocRequest` (24 bytes)
//! - Server allocates AHB, exports dmabuf fd, tracks in AhbBufferTracker
//! - Server sends `AllocResponse` (32 bytes) + dmabuf fd via SCM_RIGHTS
//! - Client uses the fd to render via Turnip, then commits via Wayland

use crate::android::utils::socket::create_unix_listener;

use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use smithay::backend::allocator::{Allocator, Fourcc, Modifier};
use smithay::backend::allocator::dmabuf::AsDmabuf;

use super::ahb_allocator::{AhbAllocator, AhbBufferTracker};

// ── Wire protocol ──────────────────────────────────────────────────────────

const MSG_ALLOC: u32 = 1;
const MSG_DESTROY: u32 = 2;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct AllocRequest {
    msg_type: u32,
    width: u32,
    height: u32,
    format: u32,  // DRM fourcc
    flags: u32,
    _pad: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct AllocResponse {
    success: u32,
    width: u32,
    height: u32,
    stride: u32,
    format: u32,
    _pad: u32,
    modifier: u64,
}

// ── Server ─────────────────────────────────────────────────────────────────

/// Shared state between the server thread and the compositor.
pub struct GbmServerState {
    pub allocator: AhbAllocator,
    pub tracker: AhbBufferTracker,
}

/// Start the GBM allocator server in a background thread.
/// Returns the shared state (for the compositor to query tracked buffers).
pub fn start_server(rootfs: &str) -> Arc<Mutex<GbmServerState>> {
    let socket_path = PathBuf::from(format!("{rootfs}/tmp/gbm-alloc-0"));

    let listener = match create_unix_listener(&socket_path) {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("[gbm-server] Failed to bind {}: {e}", socket_path.display());
            // Return state anyway so compositor can start without the server
            return Arc::new(Mutex::new(GbmServerState {
                allocator: AhbAllocator::new(),
                tracker: AhbBufferTracker::new(),
            }));
        }
    };

    tracing::info!("[gbm-server] Listening at {}", socket_path.display());

    let state = Arc::new(Mutex::new(GbmServerState {
        allocator: AhbAllocator::new(),
        tracker: AhbBufferTracker::new(),
    }));

    let state_clone = state.clone();
    std::thread::Builder::new()
        .name("gbm-server".into())
        .spawn(move || server_loop(listener, state_clone))
        .ok();

    state
}

fn server_loop(listener: UnixListener, state: Arc<Mutex<GbmServerState>>) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let state = state.clone();
                std::thread::Builder::new()
                    .name("gbm-client".into())
                    .spawn(move || handle_client(stream, state))
                    .ok();
            }
            Err(e) => {
                tracing::error!("[gbm-server] Accept failed: {e}");
            }
        }
    }
}

fn handle_client(
    stream: std::os::unix::net::UnixStream,
    state: Arc<Mutex<GbmServerState>>,
) {
    tracing::info!("[gbm-server] Client connected");

    loop {
        // Read request
        let mut req = AllocRequest {
            msg_type: 0, width: 0, height: 0, format: 0, flags: 0, _pad: 0,
        };
        let req_bytes = unsafe {
            std::slice::from_raw_parts_mut(
                &mut req as *mut AllocRequest as *mut u8,
                std::mem::size_of::<AllocRequest>(),
            )
        };

        match recv_bytes(stream.as_raw_fd(), req_bytes) {
            Ok(0) => {
                tracing::info!("[gbm-server] Client disconnected");
                return;
            }
            Ok(n) if n < std::mem::size_of::<AllocRequest>() => {
                tracing::warn!("[gbm-server] Short read: {n} bytes");
                return;
            }
            Err(e) => {
                tracing::error!("[gbm-server] Read error: {e}");
                return;
            }
            Ok(_) => {}
        }

        match req.msg_type {
            MSG_ALLOC => {
                handle_alloc(&stream, &state, &req);
            }
            MSG_DESTROY => {
                tracing::debug!("[gbm-server] Destroy (no-op, buffers freed by client fd close)");
            }
            other => {
                tracing::warn!("[gbm-server] Unknown message type: {other}");
            }
        }
    }
}

fn handle_alloc(
    stream: &std::os::unix::net::UnixStream,
    state: &Arc<Mutex<GbmServerState>>,
    req: &AllocRequest,
) {
    let fourcc = Fourcc::try_from(req.format).unwrap_or(Fourcc::Abgr8888);
    tracing::info!("[gbm-server] Alloc {}x{} fmt={:?} flags=0x{:x}",
        req.width, req.height, fourcc, req.flags);

    let mut guard = match state.lock() {
        Ok(g) => g,
        Err(e) => {
            tracing::error!("[gbm-server] Lock poisoned: {e}");
            send_error(stream);
            return;
        }
    };

    // Allocate AHB
    let buffer = match guard.allocator.create_buffer(
        req.width, req.height, fourcc, &[Modifier::Invalid],
    ) {
        Ok(b) => b,
        Err(e) => {
            tracing::error!("[gbm-server] Alloc failed: {e}");
            send_error(stream);
            return;
        }
    };

    let stride = buffer.stride;
    let format = buffer.format;

    // Export as dmabuf and track
    let dmabuf = match guard.tracker.track(buffer) {
        Ok(d) => d,
        Err(e) => {
            tracing::error!("[gbm-server] Export failed: {e}");
            send_error(stream);
            return;
        }
    };

    // Get the dmabuf fd to send to client
    let dmabuf_fd = match dmabuf.handles().next() {
        Some(fd) => {
            // Dup the fd for the client
            let raw = fd.as_raw_fd();
            let duped = unsafe { libc::dup(raw) };
            if duped < 0 {
                tracing::error!("[gbm-server] dup failed");
                send_error(stream);
                return;
            }
            duped
        }
        None => {
            tracing::error!("[gbm-server] No fd in dmabuf");
            send_error(stream);
            return;
        }
    };

    let resp = AllocResponse {
        success: 1,
        width: req.width,
        height: req.height,
        stride,
        format: format.code as u32,
        _pad: 0,
        modifier: format.modifier.into(),
    };

    let resp_bytes = unsafe {
        std::slice::from_raw_parts(
            &resp as *const AllocResponse as *const u8,
            std::mem::size_of::<AllocResponse>(),
        )
    };

    if let Err(e) = send_with_fd(stream.as_raw_fd(), resp_bytes, dmabuf_fd) {
        tracing::error!("[gbm-server] Send failed: {e}");
    }

    // Close our copy of the dup'd fd (client owns it now)
    unsafe { libc::close(dmabuf_fd) };

    tracing::info!("[gbm-server] Allocated {}x{} stride={} modifier={:#x} → fd sent",
        req.width, req.height, stride, u64::from(format.modifier));
}

fn send_error(stream: &std::os::unix::net::UnixStream) {
    let resp = AllocResponse {
        success: 0, width: 0, height: 0, stride: 0, format: 0, _pad: 0, modifier: 0,
    };
    let resp_bytes = unsafe {
        std::slice::from_raw_parts(
            &resp as *const AllocResponse as *const u8,
            std::mem::size_of::<AllocResponse>(),
        )
    };
    let _ = send_bytes(stream.as_raw_fd(), resp_bytes);
}

// ── Low-level socket helpers (SCM_RIGHTS) ──────────────────────────────────

fn recv_bytes(fd: RawFd, buf: &mut [u8]) -> Result<usize, std::io::Error> {
    let n = unsafe {
        libc::recv(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len(), 0)
    };
    if n < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(n as usize)
    }
}

fn send_bytes(fd: RawFd, buf: &[u8]) -> Result<(), std::io::Error> {
    let n = unsafe {
        libc::send(fd, buf.as_ptr() as *const libc::c_void, buf.len(), 0)
    };
    if n < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn send_with_fd(sock_fd: RawFd, data: &[u8], fd_to_send: RawFd) -> Result<(), std::io::Error> {
    // Build cmsg for SCM_RIGHTS
    let mut cmsg_buf = [0u8; unsafe { libc::CMSG_SPACE(std::mem::size_of::<i32>() as u32) } as usize];

    let mut iov = libc::iovec {
        iov_base: data.as_ptr() as *mut libc::c_void,
        iov_len: data.len(),
    };

    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = cmsg_buf.as_mut_ptr() as *mut libc::c_void;
    msg.msg_controllen = cmsg_buf.len() as _;

    let cmsg = unsafe { libc::CMSG_FIRSTHDR(&msg) };
    unsafe {
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<i32>() as u32) as _;
        std::ptr::copy_nonoverlapping(
            &fd_to_send as *const i32 as *const u8,
            libc::CMSG_DATA(cmsg),
            std::mem::size_of::<i32>(),
        );
    }

    let n = unsafe { libc::sendmsg(sock_fd, &msg, 0) };
    if n < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}
