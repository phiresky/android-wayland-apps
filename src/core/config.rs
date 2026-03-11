use serde::{Deserialize, Serialize};
use std::fs;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const ARCH_FS_ROOT: &str = "/data/data/io.github.phiresky.wayland_android/files/arch";
pub const WAYLAND_SOCKET_NAME: &str = "wayland-0";
pub const CONFIG_FILE: &str = "/etc/launcher/launcher.toml";

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct LocalConfig {
    #[serde(default)]
    pub user: UserConfig,

    #[serde(default)]
    pub command: CommandConfig,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UserConfig {
    pub username: String,
}

impl Default for UserConfig {
    fn default() -> Self {
        Self {
            username: "root".to_string(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CommandConfig {
    #[serde(default = "default_launch")]
    pub launch: String,
}

pub fn default_launch_command() -> String {
    default_launch()
}

fn default_launch() -> String {
    "XDG_RUNTIME_DIR=/tmp WAYLAND_DISPLAY=wayland-0 weston-terminal".to_string()
}

impl Default for CommandConfig {
    fn default() -> Self {
        Self {
            launch: default_launch(),
        }
    }
}

pub fn parse_config(full_config_path: String) -> LocalConfig {
    if let Ok(content) = fs::read_to_string(&full_config_path) {
        if let Ok(config) = toml::from_str::<LocalConfig>(&content) {
            return config;
        }
    }
    LocalConfig::default()
}
