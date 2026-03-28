#!/usr/bin/env bash
set -euo pipefail

PKG=io.github.phiresky.wayland_android
ACTIVITY="$PKG/.MainActivity"

# Kill existing instance
adb shell am force-stop "$PKG" 2>/dev/null || true

# Clear old logs then start
adb logcat -c
adb shell am start -n "$ACTIVITY"

# Stream logs: tracing uses module path as tag (e.g. android_wayland_launcher::android::compositor),
# plus stdout/stderr and crashes.
timeout 10 adb logcat -v time \
    -e 'android_wayland_launcher|RustStdoutStderr|AndroidRuntime|ActivityManager' || true
