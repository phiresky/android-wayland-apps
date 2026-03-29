//! Service and driver setup for proot.
//!
//! D-Bus, PipeWire, XDG Desktop Portal, Flatpak repo, and the hybris
//! Vulkan ICD all need configuration to work inside proot.
//! Each function is idempotent and skips if already configured.

use super::setup::setup_log;
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
    let script = r#"#!/bin/sh
# Start D-Bus system and session buses for proot (idempotent).
# Source this: . start-dbus

# XDG_RUNTIME_DIR must be user-private (dbus requires mode 700)
_uid=$(id -u)
export XDG_RUNTIME_DIR="/tmp/runtime-${_uid}"
mkdir -p "$XDG_RUNTIME_DIR" 2>/dev/null
chmod 700 "$XDG_RUNTIME_DIR" 2>/dev/null

# System bus — verify it's actually connectable (socket may be stale)
if ! dbus-send --system --dest=org.freedesktop.DBus /org/freedesktop/DBus org.freedesktop.DBus.GetId >/dev/null 2>&1; then
    rm -f /run/dbus/system_bus_socket /run/dbus/pid
    mkdir -p /run/dbus 2>/dev/null
    dbus-daemon --config-file=/etc/dbus-1/proot-system.conf --nofork --nopidfile &
    # Brief wait for socket to appear
    _i=0; while [ "$_i" -lt 10 ] && [ ! -S /run/dbus/system_bus_socket ]; do
        sleep 0.05; _i=$((_i+1))
    done
fi

# Session bus with anonymous auth (works across proot instances)
export DBUS_SESSION_BUS_ADDRESS="unix:path=/tmp/dbus-session-bus-socket"
if ! dbus-send --session --dest=org.freedesktop.DBus /org/freedesktop/DBus org.freedesktop.DBus.GetId >/dev/null 2>&1; then
    rm -f /tmp/dbus-session-bus-socket
    dbus-daemon --config-file=/etc/dbus-1/proot-session.conf --nofork --nopidfile &
    _i=0; while [ "$_i" -lt 10 ] && [ ! -S /tmp/dbus-session-bus-socket ]; do
        sleep 0.05; _i=$((_i+1))
    done
fi

# Symlink Wayland and PipeWire sockets into the new XDG_RUNTIME_DIR
for _sock in wayland-0 pipewire-0; do
    [ -S "/tmp/$_sock" ] && [ ! -e "$XDG_RUNTIME_DIR/$_sock" ] && \
        ln -sf "/tmp/$_sock" "$XDG_RUNTIME_DIR/$_sock" 2>/dev/null
done
"#;
    if let Err(e) = fs::write(&start_dbus, script) {
        tracing::error!("[setup] Failed to write start-dbus: {}", e);
        return;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&start_dbus, fs::Permissions::from_mode(0o755));
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
    let script = r##"#!/usr/bin/env python3
"""
Standalone XDG Desktop Portal for Android.

Implements org.freedesktop.portal.FileChooser directly on the session bus,
bypassing xdg-desktop-portal (which needs /proc access that proot can't provide).
Forwards file chooser requests to the Android compositor via a Unix socket.
"""

import dbus
import dbus.service
import dbus.mainloop.glib
import json
import socket
import sys
import threading
from gi.repository import GLib

SOCKET_PATH = "/tmp/.portal-bridge"
BUS_NAME = "org.freedesktop.portal.Desktop"
OBJECT_PATH = "/org/freedesktop/portal/desktop"

request_counter = 0
main_loop = None


def send_portal_request(request):
    """Send a JSON request to the compositor and wait for response."""
    try:
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        sock.connect(SOCKET_PATH)
        sock.settimeout(300)
        sock.sendall((json.dumps(request) + "\n").encode())
        buf = b""
        while b"\n" not in buf:
            chunk = sock.recv(4096)
            if not chunk:
                break
            buf += chunk
        sock.close()
        if buf:
            return json.loads(buf.decode().strip())
    except Exception as e:
        print(f"Portal bridge error: {e}", file=sys.stderr)
    return {"response": 2, "uris": []}


class RequestObject(dbus.service.Object):
    """Represents a portal request. Emits Response signal when done."""

    def __init__(self, bus, path):
        super().__init__(bus, path)

    @dbus.service.signal("org.freedesktop.portal.Request", signature="ua{sv}")
    def Response(self, response, results):
        pass

    @dbus.service.method("org.freedesktop.portal.Request", in_signature="", out_signature="")
    def Close(self):
        self.remove_from_connection()


class PortalService(dbus.service.Object):
    """Implements org.freedesktop.portal.FileChooser (frontend interface)."""

    def __init__(self, bus, path):
        super().__init__(bus, path)
        self._bus = bus

    def _get_request_path(self, sender, options):
        global request_counter
        request_counter += 1
        token = str(options.get("handle_token", f"android{request_counter}"))
        sender_part = sender[1:].replace(".", "_")
        return f"/org/freedesktop/portal/desktop/request/{sender_part}/{token}"

    def _extract_mime_types(self, options):
        mime_types = []
        filters = options.get("filters", [])
        for f in filters:
            if len(f) >= 2:
                for pattern in f[1]:
                    if len(pattern) >= 2 and int(pattern[0]) == 1:
                        mime_types.append(str(pattern[1]))
        return mime_types or ["*/*"]

    @dbus.service.method(
        "org.freedesktop.portal.FileChooser",
        in_signature="ssa{sv}",
        out_signature="o",
        sender_keyword="sender",
    )
    def OpenFile(self, parent_window, title, options, sender=None):
        req_path = self._get_request_path(sender, options)
        req_obj = RequestObject(self._bus, req_path)
        multiple = bool(options.get("multiple", False))
        directory = bool(options.get("directory", False))
        mime_types = self._extract_mime_types(options)

        def do_request():
            result = send_portal_request({
                "type": "open_file",
                "id": req_path,
                "title": str(title),
                "multiple": multiple,
                "directory": directory,
                "mime_types": mime_types,
            })
            response = int(result.get("response", 2))
            uris = result.get("uris", [])
            results = {}
            if response == 0 and uris:
                results["uris"] = dbus.Array(uris, signature="s")
            GLib.idle_add(lambda: (req_obj.Response(dbus.UInt32(response), results),
                                   req_obj.remove_from_connection()))

        threading.Thread(target=do_request, daemon=True).start()
        return dbus.ObjectPath(req_path)

    @dbus.service.method(
        "org.freedesktop.portal.FileChooser",
        in_signature="ssa{sv}",
        out_signature="o",
        sender_keyword="sender",
    )
    def SaveFile(self, parent_window, title, options, sender=None):
        req_path = self._get_request_path(sender, options)
        req_obj = RequestObject(self._bus, req_path)
        current_name = str(options.get("current_name", ""))
        mime_types = self._extract_mime_types(options)

        def do_request():
            result = send_portal_request({
                "type": "save_file",
                "id": req_path,
                "title": str(title),
                "multiple": False,
                "directory": False,
                "mime_types": mime_types,
                "current_name": current_name,
            })
            response = int(result.get("response", 2))
            uris = result.get("uris", [])
            results = {}
            if response == 0 and uris:
                results["uris"] = dbus.Array(uris, signature="s")
            GLib.idle_add(lambda: (req_obj.Response(dbus.UInt32(response), results),
                                   req_obj.remove_from_connection()))

        threading.Thread(target=do_request, daemon=True).start()
        return dbus.ObjectPath(req_path)

    # Settings interface — exposes Android's color scheme to Linux apps

    def _get_color_scheme(self):
        """Query Android's color scheme via the compositor bridge."""
        result = send_portal_request({"type": "get_color_scheme"})
        return int(result.get("color_scheme", 0))

    @dbus.service.method(
        "org.freedesktop.portal.Settings",
        in_signature="ss",
        out_signature="v",
    )
    def ReadOne(self, namespace, key):
        if namespace == "org.freedesktop.appearance" and key == "color-scheme":
            return dbus.UInt32(self._get_color_scheme())
        raise dbus.exceptions.DBusException(
            f"Unknown setting: {namespace}.{key}",
            name="org.freedesktop.portal.Error.NotFound",
        )

    @dbus.service.method(
        "org.freedesktop.portal.Settings",
        in_signature="ss",
        out_signature="v",
    )
    def Read(self, namespace, key):
        # Deprecated method — wraps value in extra variant layer
        val = self.ReadOne(namespace, key)
        return dbus.types.Variant(val)

    @dbus.service.method(
        "org.freedesktop.portal.Settings",
        in_signature="as",
        out_signature="a{sa{sv}}",
    )
    def ReadAll(self, namespaces):
        result = {}
        # If no filter or matching filter, include appearance settings
        if not namespaces or any(
            ns in ("org.freedesktop.appearance", "org.freedesktop.*", "*")
            for ns in namespaces
        ):
            result["org.freedesktop.appearance"] = {
                "color-scheme": dbus.UInt32(self._get_color_scheme()),
            }
        return result

    @dbus.service.signal(
        "org.freedesktop.portal.Settings",
        signature="ssv",
    )
    def SettingChanged(self, namespace, key, value):
        pass

    # Properties interface — apps query portal versions
    @dbus.service.method(
        dbus.PROPERTIES_IFACE,
        in_signature="ss",
        out_signature="v",
    )
    def Get(self, interface, prop):
        if prop == "version":
            if interface == "org.freedesktop.portal.Settings":
                return dbus.UInt32(2)
            return dbus.UInt32(4)
        raise dbus.exceptions.DBusException(
            f"Unknown property: {interface}.{prop}",
            name="org.freedesktop.DBus.Error.UnknownProperty",
        )

    @dbus.service.method(
        dbus.PROPERTIES_IFACE,
        in_signature="s",
        out_signature="a{sv}",
    )
    def GetAll(self, interface):
        if interface == "org.freedesktop.portal.Settings":
            return {"version": dbus.UInt32(2)}
        return {"version": dbus.UInt32(4)}


def apply_color_scheme(scheme):
    """Apply color scheme to gsettings so GTK3/GTK4 apps update instantly."""
    import subprocess
    try:
        # GTK4 / GNOME 42+
        cs = {1: "prefer-dark", 2: "prefer-light"}.get(scheme, "default")
        subprocess.run(
            ["gsettings", "set", "org.gnome.desktop.interface", "color-scheme", cs],
            timeout=5, capture_output=True,
        )
        # GTK3 — theme name variant
        theme = "Adwaita-dark" if scheme == 1 else "Adwaita"
        subprocess.run(
            ["gsettings", "set", "org.gnome.desktop.interface", "gtk-theme", theme],
            timeout=5, capture_output=True,
        )
    except Exception as e:
        print(f"gsettings error: {e}", file=sys.stderr)


def poll_color_scheme(portal):
    """Poll Android color scheme and emit SettingChanged + update gsettings."""
    last_scheme = portal._get_color_scheme()
    apply_color_scheme(last_scheme)
    while True:
        import time
        time.sleep(5)
        try:
            scheme = portal._get_color_scheme()
            if scheme != last_scheme:
                last_scheme = scheme
                print(f"Color scheme changed to {scheme}", flush=True)
                apply_color_scheme(scheme)
                GLib.idle_add(
                    portal.SettingChanged,
                    "org.freedesktop.appearance",
                    "color-scheme",
                    dbus.UInt32(scheme),
                )
        except Exception as e:
            print(f"Poll error: {e}", file=sys.stderr)


def main():
    dbus.mainloop.glib.DBusGMainLoop(set_as_default=True)
    bus = dbus.SessionBus()
    bus_name = dbus.service.BusName(BUS_NAME, bus, replace_existing=True, allow_replacement=True)
    portal = PortalService(bus, OBJECT_PATH)
    # Poll for color scheme changes in background
    threading.Thread(target=poll_color_scheme, args=(portal,), daemon=True).start()
    print(f"Android portal running on {BUS_NAME}", flush=True)
    GLib.MainLoop().run()


if __name__ == "__main__":
    main()
"##;
    if let Err(e) = fs::write(&backend_script, script) {
        tracing::error!("[setup] Failed to write portal daemon: {e}");
        return;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&backend_script, fs::Permissions::from_mode(0o755));
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
