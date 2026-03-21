// TODO: Make these configurable via the Android UI (Milestone 7).

use std::sync::atomic::{AtomicBool, Ordering};

static PIPEWIRE_ENABLED: AtomicBool = AtomicBool::new(false);

pub fn pipewire_enabled() -> bool {
    PIPEWIRE_ENABLED.load(Ordering::Relaxed)
}

pub fn set_pipewire_enabled(enabled: bool) {
    PIPEWIRE_ENABLED.store(enabled, Ordering::Relaxed);
}

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const ARCH_FS_ROOT: &str = "/data/data/io.github.phiresky.wayland_android/files/arch";
pub const ARCH_FS_ARCHIVE: &str = "https://github.com/termux/proot-distro/releases/download/v4.34.2/archlinux-aarch64-pd-v4.34.2.tar.xz";
pub const WAYLAND_SOCKET_NAME: &str = "wayland-0";

pub const USERNAME: &str = "alarm";

/// Packages to install in the Arch rootfs during setup.
pub const PACKAGES: &[&str] = &[
    "ca-certificates",
    "gedit",
    "mesa-utils",
    "pipewire",
    "wireplumber",
    "qt6-wayland",
    "shared-mime-info",
    "gdk-pixbuf2",
    "noto-fonts",
    "noto-fonts-emoji",
    "python",
    "vulkan-tools",
    "sudo",
    "kitty",
    "dbus-python",
    "python-gobject",
    "xdg-desktop-portal",
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

/// Extra launcher entries as (name, exec, icon) triples (apps without .desktop files).
/// Icon is the hicolor theme name (looked up in rootfs), or empty string for none.
pub const LAUNCHER_EXTRA: &[(&str, &str, &str)] = &[
    ("EGL Gears", "eglgears_wayland", "@drawable/ic_eglgears"),
    ("Vulkan Cube", "vkcube", "@drawable/ic_vkcube"),
    ("Factorio", "BOX64_DYNAREC_BIGBLOCK=2 BOX64_DYNAREC_STRONGMEM=0 BOX64_DYNAREC_FASTNAN=1 BOX64_DYNAREC_FASTROUND=1 BOX64_DYNAREC_SAFEFLAGS=0 BOX64_DYNAREC_CALLRET=1 MESA_LOADER_DRIVER_OVERRIDE=zink GALLIUM_DRIVER=zink box64 /home/alarm/factorio/bin/x64/factorio --graphics-quality=low", "factorio"),
];
