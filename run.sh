#!/usr/bin/env bash
set -euo pipefail

PKG=io.github.phiresky.wayland_android
ACTIVITY="$PKG/.MainActivity"

# Kill existing instance
adb shell am force-stop "$PKG" 2>/dev/null || true

# Clear old logs then start
adb logcat -c
adb shell am start -n "$ACTIVITY"

# Stream logs: app native (android_logger uses crate name as tag, truncated),
# smithay, stdout/stderr, plus crashes.
exec adb logcat -v time \
    AndroidRuntime:E \
    ActivityManager:W \
    RustStdoutStderr:V \
    'android_wayland_launc..:V' \
    '*:S'
