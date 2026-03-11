use serde::{Deserialize, Serialize};
use std::fs;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const ARCH_FS_ROOT: &str = "/data/data/io.github.phiresky.wayland_android/files/arch";
pub const ARCH_FS_ARCHIVE: &str = "https://github.com/termux/proot-distro/releases/download/v4.34.2/archlinux-aarch64-pd-v4.34.2.tar.xz";
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
    #[serde(default = "default_check")]
    pub check: String,
    #[serde(default = "default_install")]
    pub install: String,
    #[serde(default = "default_launch")]
    pub launch: String,
}

fn default_check() -> String {
    "sh -c 'pacman -Q weston gedit mesa-demos && test -f /usr/share/mime/mime.cache'".to_string()
}

fn default_install() -> String {
    "stdbuf -oL sh -c '(pacman -Syu --needed --noconfirm weston gedit mesa-demos && pacman -S --noconfirm shared-mime-info gdk-pixbuf2) 2>&1 | tee /tmp/install.log'".to_string()
}

fn default_launch() -> String {
    "weston-terminal".to_string()
}

impl Default for CommandConfig {
    fn default() -> Self {
        Self {
            check: default_check(),
            install: default_install(),
            launch: default_launch(),
        }
    }
}

pub fn parse_config(full_config_path: String) -> LocalConfig {
    if let Ok(content) = fs::read_to_string(&full_config_path)
        && let Ok(config) = toml::from_str::<LocalConfig>(&content) {
            return config;
        }
    LocalConfig::default()
}
