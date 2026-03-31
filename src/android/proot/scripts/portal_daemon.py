#!/usr/bin/env python3
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
