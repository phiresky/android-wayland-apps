pub mod egl;
mod event_centralizer;
mod event_handler;
mod input;
mod keymap;

pub use event_centralizer::{centralize, CentralizedEvent};
pub use event_handler::handle;
pub use egl::{bind_egl, WinitGraphicsBackend};

use smithay::{
    backend::renderer::gles::GlesRenderer,
    utils::{Clock, Monotonic},
};
use crate::android::compositor::Compositor;
use crate::android::window_manager::WindowManager;

pub struct WaylandBackend {
    pub compositor: Compositor,
    pub graphic_renderer: Option<WinitGraphicsBackend<GlesRenderer>>,
    pub window_manager: Option<WindowManager>,
    pub clock: Clock<Monotonic>,
    pub key_counter: u32,
    pub scale_factor: f64,
}
