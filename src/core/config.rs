pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const ARCH_FS_ROOT: &str = "/data/data/io.github.phiresky.wayland_android/files/arch";
pub const ARCH_FS_ARCHIVE: &str = "https://github.com/termux/proot-distro/releases/download/v4.34.2/archlinux-aarch64-pd-v4.34.2.tar.xz";
pub const WAYLAND_SOCKET_NAME: &str = "wayland-0";

pub const DEFAULT_USERNAME: &str = "root";
pub const CHECK_CMD: &str = "sh -c 'pacman -Q weston gedit mesa-demos && test -f /usr/share/mime/mime.cache'";
pub const INSTALL_CMD: &str = "stdbuf -oL sh -c '(pacman -Syu --needed --noconfirm weston gedit mesa-demos && pacman -S --noconfirm shared-mime-info gdk-pixbuf2) 2>&1 | tee /tmp/install.log'";
pub const LAUNCH_CMD: &str = "weston-terminal";
