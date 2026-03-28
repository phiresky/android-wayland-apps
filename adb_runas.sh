#!/usr/bin/env bash
# Run a command inside the app's proot Arch rootfs on the device.
# Reads proot-config.json for binds and env vars (shared with Rust code).
# Usage: ./adb_runas.sh [command...]
#        ./adb_runas.sh pacman -S mesa-demos   (runs as alarm)
#        ./adb_runas.sh                        (interactive alarm shell)
#        USER_NAME=root ./adb_runas.sh              (interactive root shell)
#        USER_NAME=root ./adb_runas.sh pacman -S mesa-demos
#        echo 'complex | cmd' | ./adb_runas.sh (stdin script, no escaping needed)
#        ./adb_runas.sh <<'EOF'                (heredoc script)
#        MOZ_ENABLE_WAYLAND=1 firefox 2>&1
#        EOF
set -euo pipefail

PKG=io.github.phiresky.wayland_android
ROOTFS=./files/arch
CONFIG="$(dirname "$0")/proot-config.json"

# Resolve native lib dir from package path
APK_DIR=$(adb shell pm path "$PKG" </dev/null | grep base.apk | head -1 | sed 's|package:||;s|/base.apk||' | tr -d '\r')
LIBDIR="$APK_DIR/lib/arm64"

# Default to alarm user; override with USER_NAME=root
PROOT_USER="${USER_NAME:-alarm}"

if [ "$PROOT_USER" = "root" ]; then
    HOMEDIR=/root
else
    HOMEDIR="/home/$PROOT_USER"
fi

# Generate proot args from JSON config using jq
PROOT_ARGS=$(jq -r '.proot_args[]' "$CONFIG")
BINDS=$(jq -r '.binds[]' "$CONFIG" | sed "s|\\\$ROOTFS|$ROOTFS|g;s|\\\$LIBDIR|$LIBDIR|g" | while read -r b; do echo "--bind=$b"; done)
BINDS_OPT=$(jq -r '.binds_if_exists[]' "$CONFIG" | sed "s|\\\$ROOTFS|$ROOTFS|g;s|\\\$LIBDIR|$LIBDIR|g")
ENV_VARS=$(jq -r '.env | to_entries[] | "\(.key)=\(.value)"' "$CONFIG" | sed "s|\\\$ROOTFS|$ROOTFS|g;s|\\\$LIBDIR|$LIBDIR|g;s|\\\$CACHE_DIR|./cache|g")

# GPU: check if kgsl exists on device (Qualcomm)
HAS_GPU=$(adb shell "test -e /dev/kgsl-3d0 && echo 1 || echo 0" </dev/null | tr -d '\r')
if [ "$HAS_GPU" = "1" ]; then
    GPU_ENV=$(jq -r '.env_if_gpu | to_entries[] | "\(.key)=\(.value)"' "$CONFIG")
else
    GPU_ENV=""
fi

SUFFIX=$$
# Write the actual command to a temp file inside proot
CMD_FILE=$ROOTFS/tmp/.proot_cmd_$SUFFIX.sh
if [ -t 0 ] && [ $# -eq 0 ]; then
    # Interactive: launch bash
    adb shell run-as "$PKG" sh -c "'echo \"exec bash -l\" > $CMD_FILE'" </dev/null
elif [ $# -gt 0 ]; then
    # Arguments: check for shell metacharacters
    case "$*" in
        *\|*|*\&*|*\;*|*\>*|*\<*|*\$*|*\"*|*\'*|*\\*)
            echo "Error: shell metacharacters in arguments are not supported." >&2
            echo "Use stdin or heredoc mode instead:" >&2
            echo "  ./adb_runas.sh <<'EOF'" >&2
            echo "  $*" >&2
            echo "  EOF" >&2
            exit 1
            ;;
    esac
    adb shell run-as "$PKG" sh -c "'echo \"exec $*\" > $CMD_FILE'" </dev/null
else
    # Stdin: pipe the script
    adb shell run-as "$PKG" sh -c "'cat > $CMD_FILE'" < /dev/stdin
fi

if [ "$PROOT_USER" = "root" ]; then
    SHELL_CMD="sh /tmp/.proot_cmd_$SUFFIX.sh"
else
    SHELL_CMD="runuser -u $PROOT_USER -- sh /tmp/.proot_cmd_$SUFFIX.sh"
fi

# Build the launcher script that runs proot with all args from config
LAUNCHER=./files/.proot_launcher_$SUFFIX.sh
{
    echo "#!/bin/sh"
    echo "export PROOT_LOADER=$LIBDIR/libproot_loader.so"
    echo "export PROOT_TMP_DIR=./cache/proot"
    # optional binds: check existence on device before adding
    echo 'BIND_OPT=""'
    echo "$BINDS_OPT" | while read -r entry; do
        [ -z "$entry" ] && continue
        src="${entry%%:*}"
        echo "[ -e $src ] && BIND_OPT=\"\$BIND_OPT --bind=$entry\""
    done
    # Start proot command
    printf "exec %s \\\\\n" "$LIBDIR/libproot.so"
    printf "    -r %s \\\\\n" "$ROOTFS"
    printf "    -w %s \\\\\n" "$HOMEDIR"
    # proot_args from config
    for arg in $PROOT_ARGS; do
        printf "    %s \\\\\n" "$arg"
    done
    printf "    --kill-on-exit \\\\\n"
    # binds from config
    echo "$BINDS" | while read -r b; do [ -n "$b" ] && printf "    %s \\\\\n" "$b"; done
    # optional binds (checked at runtime on device)
    echo "    \$BIND_OPT \\"
    # /usr/bin/env -i
    printf "    /usr/bin/env -i \\\\\n"
    printf "    HOME=%s \\\\\n" "$HOMEDIR"
    printf "    USER=%s \\\\\n" "$PROOT_USER"
    printf "    LOGNAME=%s \\\\\n" "$PROOT_USER"
    # env vars from config
    echo "$ENV_VARS" | while read -r e; do [ -n "$e" ] && printf "    %s \\\\\n" "$e"; done
    # GPU env vars
    if [ -n "$GPU_ENV" ]; then
        echo "$GPU_ENV" | while read -r e; do [ -n "$e" ] && printf "    %s \\\\\n" "$e"; done
    fi
    # final command
    printf "    %s\n" "$SHELL_CMD"
} | adb shell run-as "$PKG" sh -c "'cat > $LAUNCHER'"

# Run with -t for PTY allocation when interactive
# rlwrap provides local readline (arrow keys, history, Ctrl+R) since
# run-as UID switch prevents tcsetattr on adb's PTY
if [ -t 0 ]; then
    if [ "${USE_RLWRAP:-0}" = 1 ]; then
        rlwrap -a adb shell -t run-as "$PKG" sh "$LAUNCHER"
    else
        adb shell -t run-as "$PKG" sh "$LAUNCHER"
    fi
else
    adb shell run-as "$PKG" sh "$LAUNCHER"
fi
