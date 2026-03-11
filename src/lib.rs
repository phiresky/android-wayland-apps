pub mod core {
    pub mod config;
}

#[cfg(target_os = "android")]
pub mod android {
    pub mod main;
    pub mod app;
    pub mod compositor;
    pub mod backend;
    pub mod proot;
    pub mod utils;
}
