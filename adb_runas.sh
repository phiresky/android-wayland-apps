#!/usr/bin/env bash
# Run a command inside the app's proot Arch rootfs on the device.
# Runs as alarm user by default, set USERNAME=root for root.
# Usage: ./adb_runas.sh [command...]
#        ./adb_runas.sh pacman -S mesa-demos   (runs as alarm)
#        ./adb_runas.sh                        (interactive alarm shell)
#        USERNAME=root ./adb_runas.sh              (interactive root shell)
#        USERNAME=root ./adb_runas.sh pacman -S mesa-demos
#        echo 'complex | cmd' | ./adb_runas.sh (stdin script, no escaping needed)
#        ./adb_runas.sh <<'EOF'                (heredoc script)
#        MOZ_ENABLE_WAYLAND=1 firefox 2>&1
#        EOF
set -euo pipefail

PKG=io.github.phiresky.wayland_android
ROOTFS=./files/arch

# Resolve native lib dir from package path
APK_DIR=$(adb shell pm path "$PKG" </dev/null | grep base.apk | head -1 | sed 's|package:||;s|/base.apk||' | tr -d '\r')
LIBDIR="$APK_DIR/lib/arm64"

# Default to alarm user; override with USERNAME=root
PROOT_USER="${USERNAME:-alarm}"

if [ "$PROOT_USER" = "root" ]; then
    HOMEDIR=/root
else
    HOMEDIR="/home/$PROOT_USER"
fi

# Write the actual command to a script file inside the rootfs.
# Stdin mode: pipe or heredoc content is used verbatim (no escaping needed).
# Args mode: args are encoded with printf %q so special chars survive intact.
SUFFIX=$(head -c4 /dev/urandom | xxd -p)
CMD_FILE=$ROOTFS/tmp/.proot_cmd_$SUFFIX.sh
USE_RLWRAP=0
if [ $# -eq 0 ]; then
    if [ -t 0 ]; then
        # Interactive: launch a shell
        if command -v rlwrap &>/dev/null; then
            USE_RLWRAP=1
            CMD_CONTENT="exec bash --noediting -li"
        else
            CMD_CONTENT="exec bash -li"
        fi
    else
        # Stdin is piped/heredoc: read verbatim as the script
        CMD_CONTENT=$(cat)
    fi
else
    # Reject shell metacharacters in args mode — use stdin/heredoc for complex commands
    for arg in "$@"; do
        case "$arg" in
            *\&\&*|*\|\|*|*\;*|*\>*|*\<*|*\|*|*\`*|*\$\(*)
                echo "Error: shell metacharacters in arguments are not supported." >&2
                echo "Use stdin or heredoc mode instead:" >&2
                echo "  ./adb_runas.sh <<'EOF'" >&2
                echo "  $*" >&2
                echo "  EOF" >&2
                exit 1
                ;;
        esac
    done
    # printf %q properly quotes each arg; the result is a valid sh command line
    CMD_CONTENT=$(printf '%q ' "$@")
fi

adb shell run-as "$PKG" sh -c "'cat > $CMD_FILE'" << CMDEOF
#!/bin/sh
[ -f /usr/local/bin/start-dbus ] && . /usr/local/bin/start-dbus 2>/dev/null || true
$CMD_CONTENT
CMDEOF

# adb shell run-as "$PKG" cat $CMD_FILE


# Build the shell invocation for the launcher
if [ "$PROOT_USER" = "root" ]; then
    SHELL_CMD="sh /tmp/.proot_cmd_$SUFFIX.sh"
else
    SHELL_CMD="runuser -u $PROOT_USER -- sh /tmp/.proot_cmd_$SUFFIX.sh"
fi

# Write a launcher script to the device so adb shell gets a simple command
# (complex quoted commands prevent proper PTY/raw-mode handling)
LAUNCHER=./files/.proot_launcher_$SUFFIX.sh
adb shell run-as "$PKG" sh -c "'cat > $LAUNCHER'" << EOF
#!/bin/sh
export PROOT_LOADER=$LIBDIR/libproot_loader.so
export PROOT_TMP_DIR=./cache/proot
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
    --bind=$LIBDIR:$LIBDIR \
    --bind=/system:/system \
    --bind=/apex:/apex \
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
    _PROOT_BIN=$LIBDIR/libproot.so \
    _PROOT_LOADER=$LIBDIR/libproot_loader.so \
    _PROOT_TMP_DIR=./cache/proot \
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
