#!/usr/bin/env bash
# Run a command inside the app's proot Arch rootfs on the device.
# Usage: ./adb_runas.sh [user] [command...]
#        ./adb_runas.sh alarm pacman -S mesa-demos
#        ./adb_runas.sh alarm sh -c 'cmd1 && cmd2'
#        ./adb_runas.sh pacman -S mesa-demos   (runs as root)
#        ./adb_runas.sh                        (interactive root shell)
#        ./adb_runas.sh alarm                  (interactive alarm shell)
set -euo pipefail

PKG=io.github.phiresky.wayland_android
ROOTFS=./files/arch

# Resolve native lib dir from package path
APK_DIR=$(adb shell pm path "$PKG" | grep base.apk | head -1 | sed 's|package:||;s|/base.apk||' | tr -d '\r')
LIBDIR="$APK_DIR/lib/arm64"

# Check if first arg is a known user (alarm) or root
PROOT_USER=root
if [ $# -gt 0 ] && [ "$1" = "alarm" ]; then
    PROOT_USER="$1"
    shift
fi

if [ "$PROOT_USER" = "root" ]; then
    HOMEDIR=/root
else
    HOMEDIR="/home/$PROOT_USER"
fi

# Write the actual command to a script file inside the rootfs.
# This avoids nested sh -c quoting issues — args are encoded with printf %q
# so special characters survive the heredoc expansion intact.
CMD_FILE=$ROOTFS/tmp/.proot_cmd.sh
USE_RLWRAP=0
if [ $# -eq 0 ]; then
    if [ -t 0 ] && command -v rlwrap &>/dev/null; then
        USE_RLWRAP=1
        CMD_CONTENT="exec bash --noediting -li"
    else
        CMD_CONTENT="exec bash -li"
    fi
else
    # printf %q properly quotes each arg; the result is a valid sh command line
    CMD_CONTENT=$(printf '%q ' "$@")
fi

adb shell run-as "$PKG" sh -c "'cat > $CMD_FILE'" << CMDEOF
#!/bin/sh
$CMD_CONTENT
CMDEOF

# Build the shell invocation for the launcher
if [ "$PROOT_USER" = "root" ]; then
    SHELL_CMD="sh /tmp/.proot_cmd.sh"
else
    SHELL_CMD="runuser -u $PROOT_USER -- sh /tmp/.proot_cmd.sh"
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
    -w $HOMEDIR \
    --bind=/dev \
    --bind=/proc \
    --bind=/sys \
    --bind=/storage/emulated/0:/storage/emulated/0 \
    --bind=/storage/emulated/0:/sdcard \
    --bind=/data/local/tmp \
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
    HOME=$HOMEDIR \
    LANG=C.UTF-8 \
    TERM=xterm-256color \
    PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    TMPDIR=/tmp \
    USER=$PROOT_USER \
    LOGNAME=$PROOT_USER \
    XDG_RUNTIME_DIR=/tmp \
    WAYLAND_DISPLAY=wayland-0 \
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
