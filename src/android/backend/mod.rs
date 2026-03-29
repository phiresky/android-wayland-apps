#[cfg(feature = "zero-copy")]
pub mod ahb_allocator;
pub mod egl;
#[cfg(feature = "zero-copy")]
pub mod gbm_server;
pub mod vulkan_renderer;
pub mod surface_transaction;
mod event_handler;
mod render;
mod input;
mod ime;
pub(crate) mod keymap;

pub use event_handler::{compositor_tick, dispatch_wayland};
pub use egl::{init_egl_headless, CompositorRenderer};

use crate::android::compositor::Compositor;
use crate::android::window_manager::WindowManager;

use std::os::unix::io::RawFd;
use std::sync::{Arc, Mutex};

pub struct WaylandBackend {
    pub compositor: Compositor,
    pub renderer: Option<CompositorRenderer>,
    pub vk_renderer: Option<vulkan_renderer::VulkanRenderer>,
    pub window_manager: Option<WindowManager>,
    pub wake_fd: RawFd,
    pub scale_factor: f64,
    /// Shared state from GBM allocator server (zero-copy path).
    #[cfg(feature = "zero-copy")]
    pub gbm_state: Option<Arc<Mutex<gbm_server::GbmServerState>>>,
}

/// Signal the compositor thread to wake up via eventfd.
pub fn signal_wake(fd: RawFd) {
    let val: u64 = 1;
    unsafe { libc::write(fd, &val as *const u64 as *const libc::c_void, 8) };
}

/// Drain the eventfd after waking.
pub fn drain_wake(fd: RawFd) {
    let mut val: u64 = 0;
    unsafe { libc::read(fd, &mut val as *mut u64 as *mut libc::c_void, 8) };
}
