#!/usr/bin/env bash
# Run a command inside the app's proot Arch rootfs on the device.
# Usage: ./adb_runas.sh pacman -S mesa-demos
#        ./adb_runas.sh           (interactive shell)
set -euo pipefail

PKG=io.github.phiresky.wayland_android
ROOTFS=./files/arch

# Resolve native lib dir from package path
APK_DIR=$(adb shell pm path "$PKG" | grep base.apk | head -1 | sed 's|package:||;s|/base.apk||' | tr -d '\r')
LIBDIR="$APK_DIR/lib/arm64"

# Interactive: run bash login shell directly; command: use sh -c
# Use --noediting when rlwrap handles readline locally (avoids broken
# tcsetattr through run-as UID mismatch)
if [ $# -eq 0 ]; then
    if [ -t 0 ] && command -v rlwrap &>/dev/null; then
        SHELL_CMD="bash --noediting -li"
        USE_RLWRAP=1
    else
        SHELL_CMD="bash -li"
        USE_RLWRAP=0
    fi
else
    SHELL_CMD="sh -c \"$*\""
    USE_RLWRAP=0
fi

# Write a launcher script to the device so adb shell gets a simple command
# (complex quoted commands prevent proper PTY/raw-mode handling)
LAUNCHER=./files/.proot_launcher.sh
adb shell run-as "$PKG" sh -c "'cat > $LAUNCHER'" << EOF
#!/bin/sh
export PROOT_LOADER=$LIBDIR/libproot_loader.so
export PROOT_TMP_DIR=./files
exec $LIBDIR/libproot.so \
    -r $ROOTFS \
    -L \
    --link2symlink \
    --sysvipc \
    --kill-on-exit \
    --root-id \
    -w /root \
    --bind=/dev \
    --bind=/proc \
    --bind=/sys \
    --bind=$ROOTFS/tmp:/dev/shm \
    --bind=/dev/urandom:/dev/random \
    --bind=/proc/self/fd:/dev/fd \
    --bind=$ROOTFS/proc/.loadavg:/proc/loadavg \
    --bind=$ROOTFS/proc/.stat:/proc/stat \
    --bind=$ROOTFS/proc/.uptime:/proc/uptime \
    --bind=$ROOTFS/proc/.version:/proc/version \
    --bind=$ROOTFS/proc/.vmstat:/proc/vmstat \
    --bind=$ROOTFS/proc/.sysctl_entry_cap_last_cap:/proc/sys/kernel/cap_last_cap \
    --bind=$ROOTFS/proc/.sysctl_inotify_max_user_watches:/proc/sys/fs/inotify/max_user_watches \
    --bind=$ROOTFS/sys/.empty:/sys/fs/selinux \
    /usr/bin/env -i \
    HOME=/root \
    LANG=C.UTF-8 \
    TERM=xterm-256color \
    PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    TMPDIR=/tmp \
    USER=root \
    LOGNAME=root \
    XDG_RUNTIME_DIR=/tmp \
    $SHELL_CMD
EOF

# Run with -t for PTY allocation when interactive
# rlwrap provides local readline (arrow keys, history, Ctrl+R) since
# run-as UID switch prevents tcsetattr on adb's PTY
if [ -t 0 ]; then
    if [ "$USE_RLWRAP" = 1 ]; then
        rlwrap -a adb shell -t run-as "$PKG" sh "$LAUNCHER"
    else
        adb shell -t run-as "$PKG" sh "$LAUNCHER"
    fi
else
    adb shell run-as "$PKG" sh "$LAUNCHER"
fi