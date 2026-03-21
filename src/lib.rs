#![deny(clippy::unwrap_used, clippy::expect_used)]

pub mod core {
    pub mod config;
}

#[cfg(target_os = "android")]
pub mod android {
    pub mod main;
    pub mod app;
    pub mod compositor;
    pub mod backend;
    pub mod audio;
    pub mod camera;
    pub mod proot;
    pub mod utils;
    pub mod window_manager;
}
