#!/bin/sh
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
