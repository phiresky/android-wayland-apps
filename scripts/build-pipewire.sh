#!/bin/bash
# Cross-compile libpipewire for aarch64-linux-android (bionic)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
source "$PROJECT_DIR/.env"

PW_VERSION="1.6.1"
BUILD_DIR="$PROJECT_DIR/.tmp/pipewire-build"
SRC_DIR="$BUILD_DIR/pipewire-$PW_VERSION"
NDK="$ANDROID_NDK_HOME"
NDK_TOOLCHAIN="$NDK/toolchains/llvm/prebuilt/linux-x86_64"
API=35

mkdir -p "$BUILD_DIR"

# Download PipeWire source if not present
if [ ! -d "$SRC_DIR" ]; then
    echo "Downloading PipeWire $PW_VERSION..."
    cd "$BUILD_DIR"
    curl -L "https://gitlab.freedesktop.org/pipewire/pipewire/-/archive/$PW_VERSION/pipewire-$PW_VERSION.tar.gz" -o pipewire.tar.gz
    tar xf pipewire.tar.gz
    rm pipewire.tar.gz
fi

# Create meson cross-file
cat > "$BUILD_DIR/android-aarch64.ini" <<EOF
[binaries]
c = '$NDK_TOOLCHAIN/bin/aarch64-linux-android$API-clang'
cpp = '$NDK_TOOLCHAIN/bin/aarch64-linux-android$API-clang++'
ar = '$NDK_TOOLCHAIN/bin/llvm-ar'
strip = '$NDK_TOOLCHAIN/bin/llvm-strip'
pkg-config = '/usr/bin/false'

[host_machine]
system = 'android'
cpu_family = 'aarch64'
cpu = 'aarch64'
endian = 'little'

[built-in options]
default_library = 'shared'
c_args = ['-I$BUILD_DIR/android-stubs']
EOF

# Clean previous build
rm -rf "$BUILD_DIR/builddir"

# Patch for Android: pthread_cancel doesn't exist on bionic
if ! grep -q '__ANDROID__' "$SRC_DIR/src/pipewire/data-loop.c"; then
    sed -i 's/pthread_cancel(loop->thread);/#ifndef __ANDROID__\n\t\t\t\t\tpthread_cancel(loop->thread);\n#endif/' \
        "$SRC_DIR/src/pipewire/data-loop.c"
    echo "Patched pthread_cancel for Android"
fi

cd "$SRC_DIR"

# Configure: disable everything via auto_features, then disable remaining explicit options
meson setup "$BUILD_DIR/builddir" \
    --cross-file "$BUILD_DIR/android-aarch64.ini" \
    --prefix="$BUILD_DIR/install" \
    -Dauto_features=disabled \
    -Dspa-plugins=enabled \
    -Dsupport=enabled \
    -Ddbus=disabled \
    -Dflatpak=disabled \
    -Dpipewire-jack=disabled \
    -Dpipewire-v4l2=disabled \
    -Dsession-managers=[] \
    -Dlegacy-rtkit=false \
    -Djack-devel=false \
    -Drlimits-install=false \
    -Dpam-defaults-install=false \
    2>&1

echo "Building..."
ninja -C "$BUILD_DIR/builddir" 2>&1

echo "Copying library..."
find "$BUILD_DIR/builddir" -name "libpipewire-0.3.so" -type f | head -1 | while read f; do
    cp -v "$f" "$PROJECT_DIR/libs/arm64-v8a/"
done

echo "Done!"
