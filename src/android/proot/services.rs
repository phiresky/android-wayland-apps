//! Service and driver setup for proot.
//!
//! D-Bus, PipeWire, XDG Desktop Portal, Flatpak repo, and the hybris
//! Vulkan ICD all need configuration to work inside proot.
//! Each function is idempotent and skips if already configured.

use super::setup::{setup_log, write_executable};
use crate::core::config;
use std::{
    fs,
    os::unix::fs::symlink,
    path::Path,
};

/// Configure PipeWire for unrestricted access inside proot.
/// The default access module tries flatpak/portal checks which fail in proot,
/// causing clients (pw-cli, apps) to hang. Uncomment and set socket-based
/// access to "unrestricted" in the main config.
pub(super) fn setup_pipewire_config() {
    let conf_path = Path::new(config::ARCH_FS_ROOT)
        .join("usr/share/pipewire/pipewire.conf");
    if !conf_path.exists() {
        return;
    }
    let Ok(conf) = fs::read_to_string(&conf_path) else { return };
    let patched = conf.replace(
        "#access.socket = { pipewire-0 = \"default\", pipewire-0-manager = \"unrestricted\" }",
        "access.socket = { pipewire-0 = \"unrestricted\", pipewire-0-manager = \"unrestricted\" }",
    );
    if patched != conf {
        if let Err(e) = fs::write(&conf_path, &patched) {
            tracing::error!("[setup] Failed to patch pipewire.conf: {e}");
        }
    }
}

/// Install a custom D-Bus system bus config and a helper script for flatpak.
///
/// The default dbus system config tries to switch to user `dbus` and drop
/// capabilities, which fails inside proot. We use a `custom` bus type that
/// skips privilege dropping but listens on the standard system socket path.
///
/// The `start-dbus` script starts system + session buses and exports the
/// required environment variables. Sourced by adb_runas.sh and compositor
/// app launches.
pub fn setup_flatpak_dbus() {
    let fs_root = Path::new(config::ARCH_FS_ROOT);
    let dbus_conf = fs_root.join("etc/dbus-1/proot-system.conf");
    let start_dbus = fs_root.join("usr/local/bin/start-dbus");

    if dbus_conf.exists() && start_dbus.exists() {
        return;
    }

    setup_log("[setup] Configuring D-Bus for flatpak support...");

    // Custom system bus config that doesn't drop capabilities
    let _ = fs::create_dir_all(fs_root.join("etc/dbus-1"));
    let conf = r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>custom</type>
  <listen>unix:path=/run/dbus/system_bus_socket</listen>
  <auth>EXTERNAL</auth>
  <allow_anonymous/>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
    <allow user="*"/>
  </policy>
</busconfig>
"#;
    fs::write(&dbus_conf, conf)
        .unwrap_or_else(|e| tracing::error!("[setup] Failed to write dbus config: {}", e));

    // Custom session bus config — anonymous auth so D-Bus works across proot instances.
    // Default session config uses EXTERNAL auth which relies on SCM_CREDENTIALS,
    // but proot's ptrace interception corrupts credential ancillary data.
    let session_conf = fs_root.join("etc/dbus-1/proot-session.conf");
    let session_conf_content = r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>custom</type>
  <listen>unix:path=/tmp/dbus-session-bus-socket</listen>
  <auth>ANONYMOUS</auth>
  <allow_anonymous/>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
    <allow user="*"/>
  </policy>

  <servicedir>/usr/share/dbus-1/services</servicedir>
  <servicedir>/usr/local/share/dbus-1/services</servicedir>
</busconfig>
"#;
    fs::write(&session_conf, session_conf_content)
        .unwrap_or_else(|e| tracing::error!("[setup] Failed to write session dbus config: {}", e));

    // Helper script to start both buses (idempotent)
    let _ = fs::create_dir_all(fs_root.join("usr/local/bin"));
    if let Err(e) = write_executable(&start_dbus, include_str!("scripts/start_dbus.sh")) {
        tracing::error!("[setup] Failed to write start-dbus: {}", e);
    }
}

/// Create an empty system flatpak repo so `flatpak run` doesn't error
/// when checking /var/lib/flatpak/repo (we only use --user installs).
/// Needs a valid OSTree repo structure (config file + directories).
pub(super) fn setup_flatpak_system_repo() {
    let repo_dir = Path::new(config::ARCH_FS_ROOT).join("var/lib/flatpak/repo");
    let config_file = repo_dir.join("config");
    if config_file.exists() {
        return;
    }
    setup_log("[setup] Creating empty flatpak system repo...");
    for subdir in ["objects", "refs/heads", "refs/mirrors", "refs/remotes", "tmp", "state"] {
        let _ = fs::create_dir_all(repo_dir.join(subdir));
    }
    let config = "[core]\nrepo_version=1\nmode=bare-user-only\n";
    fs::write(&config_file, config)
        .unwrap_or_else(|e| tracing::error!("[setup] Failed to write flatpak repo config: {}", e));
}

/// Install the XDG Desktop Portal Android backend.
///
/// Sets up:
/// 1. Portal descriptor file so xdg-desktop-portal knows about our backend
/// 2. D-Bus service file for auto-activation
/// 3. The Python backend script that bridges D-Bus ↔ compositor Unix socket
/// 4. Portal config to select our backend
pub fn setup_portal() {
    let fs_root = Path::new(config::ARCH_FS_ROOT);
    let backend_script = fs_root.join("usr/local/libexec/xdg-desktop-portal-android");

    // Re-generate if script doesn't exist or uses the old backend interface
    if backend_script.exists() {
        if let Ok(content) = fs::read_to_string(&backend_script) {
            if content.contains("apply_color_scheme") {
                return; // Already has the latest version with gsettings support
            }
        }
    }

    setup_log("[setup] Installing XDG Desktop Portal Android daemon...");

    // Remove conflicting service file if xdg-desktop-portal package left one behind
    let conflict = fs_root.join("usr/share/dbus-1/services/org.freedesktop.portal.Desktop.service");
    let _ = fs::remove_file(&conflict);

    // Standalone portal daemon — implements the frontend D-Bus interface directly
    // (started explicitly from launch.rs, no D-Bus auto-activation needed)
    let _ = fs::create_dir_all(fs_root.join("usr/local/libexec"));
    if let Err(e) = write_executable(&backend_script, include_str!("scripts/portal_daemon.py")) {
        tracing::error!("[setup] Failed to write portal daemon: {e}");
    }
}

/// Install the pre-built libhybris Vulkan ICD into the proot rootfs.
/// This enables glibc apps to use Android's proprietary GPU driver directly.
/// The .so files are cross-compiled on the host via ./build-libhybris.sh.
/// Idempotent: skips if already installed.
pub fn setup_hybris_vulkan() {
    let fs_root = Path::new(config::ARCH_FS_ROOT);
    let lib_dir = fs_root.join("usr/lib");
    let icd_dir = fs_root.join("usr/share/vulkan/icd.d");

    let icd_so = lib_dir.join("libvulkan_hybris.so");
    let hybris_so = lib_dir.join("libhybris-common.so");

    if icd_so.exists() && hybris_so.exists() {
        return;
    }

    setup_log("[setup] Installing hybris Vulkan ICD...");

    let _ = fs::create_dir_all(&lib_dir);
    let _ = fs::create_dir_all(&icd_dir);

    let linker_dir = fs_root.join("usr/lib/libhybris/linker");
    let _ = fs::create_dir_all(&linker_dir);

    // Pre-built binaries from ./build-libhybris.sh (cross-compiled on host)
    let files: &[(&str, &[u8])] = &[
        ("usr/lib/libhybris-common.so.1.0.0", include_bytes!("../../../libs/arm64-v8a-linux/libhybris-common.so.1.0.0")),
        ("usr/lib/libvulkan_hybris.so", include_bytes!("../../../libs/arm64-v8a-linux/libvulkan_hybris.so")),
        // NOTE: Do NOT install the ICD JSON manifest — it causes the Khronos loader to
        // use our hybris ICD which crashes due to dual-loader dispatch table conflict.
        // Zink/Turnip should use their own ICDs. The hybris ICD is for direct dlopen only.
        ("usr/lib/libhybris/linker/q.so", include_bytes!("../../../libs/arm64-v8a-linux/libhybris-linker/q.so")),
        ("usr/lib/libhybris/linker/o.so", include_bytes!("../../../libs/arm64-v8a-linux/libhybris-linker/o.so")),
        ("usr/lib/libhybris/linker/n.so", include_bytes!("../../../libs/arm64-v8a-linux/libhybris-linker/n.so")),
        ("usr/lib/libhybris/linker/mm.so", include_bytes!("../../../libs/arm64-v8a-linux/libhybris-linker/mm.so")),
    ];

    for (path, data) in files {
        let dest = fs_root.join(path);
        if let Err(e) = fs::write(&dest, data) {
            tracing::error!("[setup] Failed to write {}: {}", path, e);
            return;
        }
    }

    // Create soname symlinks
    for (link, target) in [
        ("libhybris-common.so.1", "libhybris-common.so.1.0.0"),
        ("libhybris-common.so", "libhybris-common.so.1.0.0"),
    ] {
        let _ = std::fs::remove_file(lib_dir.join(link));
        let _ = symlink(target, lib_dir.join(link));
    }

    setup_log("[setup] hybris Vulkan ICD installed");
}
