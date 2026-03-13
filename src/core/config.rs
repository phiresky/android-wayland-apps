// TODO: Make these configurable via the Android UI (Milestone 7).

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const ARCH_FS_ROOT: &str = "/data/data/io.github.phiresky.wayland_android/files/arch";
pub const ARCH_FS_ARCHIVE: &str = "https://github.com/termux/proot-distro/releases/download/v4.34.2/archlinux-aarch64-pd-v4.34.2.tar.xz";
pub const WAYLAND_SOCKET_NAME: &str = "wayland-0";

pub const USERNAME: &str = "alarm";

/// Packages to install in the Arch rootfs during setup.
pub const PACKAGES: &[&str] = &[
    "gedit",
    "mesa-utils",
    "qt6-wayland",
    "shared-mime-info",
    "gdk-pixbuf2",
    "noto-fonts",
];

pub fn check_cmd() -> String {
    format!(
        "sh -c 'pacman -Q {} && test -f /usr/share/mime/mime.cache'",
        PACKAGES.join(" ")
    )
}

pub fn install_cmd() -> String {
    format!(
        "stdbuf -oL sh -c '(pacman -Syu --needed --noconfirm {}) 2>&1 | tee /tmp/install.log'",
        PACKAGES.join(" ")
    )
}

/// Desktop file basenames to hide from the launcher (without .desktop extension).
pub const LAUNCHER_IGNORE: &[&str] = &["avahi-discover", "bssh", "bvnc"];

/// Extra launcher entries as "name\0exec" pairs (apps without .desktop files).
pub const LAUNCHER_EXTRA: &[(&str, &str)] = &[("EGL Gears", "eglgears_wayland")];
