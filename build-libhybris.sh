#!/bin/bash
# Cross-compile libhybris-common and the Vulkan ICD for aarch64 Linux (glibc).
# Uses autotools cross-compilation (runs on host, targets aarch64-linux-gnu).
#
# Usage: ./build-libhybris.sh
# Prerequisites: clang, autoconf, automake, libtool, pkg-config
#                aarch64-linux-gnu sysroot (pacman -S aarch64-linux-gnu-gcc)
#
# Outputs to libs/arm64-v8a-linux/:
#   libhybris-common.so  — Bionic compatibility layer
#   libvulkan_hybris.so  — Vulkan ICD

set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

TARGET=aarch64-linux-gnu
SYSROOT="/usr/$TARGET"
HYBRIS="$DIR/patches/libhybris/hybris"
ICD="$DIR/hybris-vulkan-icd"
BUILD="$DIR/.tmp/libhybris-build"
OUTPUT="$DIR/libs/arm64-v8a-linux"

[ -d "$SYSROOT" ] || { echo "Missing sysroot: pacman -S aarch64-linux-gnu-gcc" >&2; exit 1; }
[ -d "$HYBRIS/common" ] || { echo "Missing submodule: git submodule update --init patches/libhybris" >&2; exit 1; }

mkdir -p "$BUILD" "$OUTPUT"

# --- Patch hooks.c fortify declarations if needed ---

grep -q 'extern int __vsprintf_chk' "$HYBRIS/common/hooks.c" 2>/dev/null || \
    sed -i '/Wrap some GCC builtin/i\
extern int __vsprintf_chk(char *s, int flag, size_t slen, const char *format, __builtin_va_list ap);\
extern int __vsnprintf_chk(char *s, size_t n, int flag, size_t slen, const char *format, __builtin_va_list ap);' "$HYBRIS/common/hooks.c"

# --- Configure ---

echo "=== Configuring libhybris ==="
(cd "$HYBRIS" && [ -f configure ] || autoreconf -fi 2>&1 | tail -3)

cd "$BUILD"
[ -f Makefile ] || \
    CC="clang --target=$TARGET --sysroot=$SYSROOT" \
    CXX="clang++ --target=$TARGET --sysroot=$SYSROOT" \
    CFLAGS="-Wno-deprecated-declarations -Wno-non-pod-varargs -Wno-implicit-function-declaration" \
    CXXFLAGS="-Wno-deprecated-declarations -Wno-non-pod-varargs" \
    "$HYBRIS/configure" --host="$TARGET" --enable-arch=arm64 \
        --with-android-headers="$ICD" --prefix=/usr --libdir=/usr/lib \
        2>&1 | tail -5

# --- Build & install ---

echo "=== Building ==="
make -j$(nproc) -C common 2>&1 | tail -5
make -C common DESTDIR="$BUILD/install" install 2>&1 | tail -3
make -C properties DESTDIR="$BUILD/install" install 2>&1 | tail -3
make -C include DESTDIR="$BUILD/install" install 2>&1 | tail -3

echo "=== Building libvulkan_hybris.so ==="
clang --target="$TARGET" --sysroot="$SYSROOT" \
    -shared -fPIC -fno-stack-protector -O2 \
    -I"$BUILD/install/usr/include" \
    -L"$BUILD/install/usr/lib" -lhybris-common \
    -Wl,-rpath,/usr/lib \
    -o "$BUILD/libvulkan_hybris.so" \
    "$ICD/vulkan_hybris_icd.c"

# --- Output ---

cp "$BUILD/install/usr/lib/libhybris-common.so"* "$OUTPUT/"
# Copy linker plugins (q.so etc.) — needed at runtime
mkdir -p "$OUTPUT/libhybris-linker"
cp "$BUILD/install/usr/lib/libhybris/linker/"*.so "$OUTPUT/libhybris-linker/" 2>/dev/null || true
cp "$BUILD/libvulkan_hybris.so" "$OUTPUT/"
llvm-strip "$OUTPUT/libhybris-common.so."* "$OUTPUT/libvulkan_hybris.so" 2>/dev/null || true

echo "=== Done ==="
ls -lh "$OUTPUT/"libhybris-common.so* "$OUTPUT/libvulkan_hybris.so"
