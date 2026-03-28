#!/bin/bash
# Build and install the hybris Vulkan ICD inside proot Arch.
# Called by the compositor's setup.rs via ArchProcess.
# Idempotent: skips if already installed and up to date.
#
# No git required — uses curl for tarballs.
# AOSP headers are copied from the device's /system partition.
set -euo pipefail

ICD_SO=/usr/lib/libvulkan_hybris.so
ICD_JSON=/usr/share/vulkan/icd.d/hybris_vulkan_icd.json
SRC_DIR=/tmp/hybris-vulkan-icd
LIBHYBRIS_DIR=/tmp/libhybris

# ── Check if already built ───────────────────────────────────────────
if [ -f "$ICD_SO" ] && [ -f "$ICD_JSON" ] && [ -f /usr/lib/libhybris-common.so ]; then
    echo "[hybris-icd] Already installed"
    exit 0
fi

echo "[hybris-icd] Installing build dependencies..."
pacman -S --needed --noconfirm clang automake autoconf libtool pkgconf vulkan-headers wayland wayland-protocols 2>&1 | tail -5

# ── Build libhybris ──────────────────────────────────────────────────
if [ ! -f /usr/lib/libhybris-common.so ]; then
    echo "[hybris-icd] Building libhybris..."

    # Extract bundled libhybris source (shipped in the APK, no network needed)
    if [ ! -d "$LIBHYBRIS_DIR/hybris/common" ]; then
        echo "[hybris-icd] Extracting bundled libhybris source..."
        mkdir -p "$LIBHYBRIS_DIR"
        tar xzf "$SRC_DIR/libhybris-src.tar.gz" -C "$LIBHYBRIS_DIR"
    fi

    cd "$LIBHYBRIS_DIR/hybris"

    # Create minimal Android config headers
    mkdir -p include/cutils include/hardware include/system
    cat > include/android-config.h << 'H'
#ifndef HYBRIS_ANDROID_CONFIG_H
#define HYBRIS_ANDROID_CONFIG_H
#define ANDROID_VERSION_MAJOR 14
#define ANDROID_VERSION_MINOR 0
#define ANDROID_VERSION_PATCH 0
#define QEMU_HARDWARE ""
#define HYBRIS_ARCH "arm64"
#endif
H
    cat > include/android-version.h << 'H'
#ifndef HYBRIS_ANDROID_VERSION_H
#define HYBRIS_ANDROID_VERSION_H
#define ANDROID_VERSION 140000
#define ANDROID_VERSION_MAJOR 14
#define ANDROID_VERSION_MINOR 0
#define ANDROID_VERSION_PATCH 0
#endif
H

    # Minimal AOSP headers — stubs sufficient for building libhybris common.
    # No network or device files needed.
    if [ ! -f include/hardware/hardware.h ]; then
        cat > include/hardware/hardware.h << 'H'
#ifndef ANDROID_HARDWARE_H
#define ANDROID_HARDWARE_H
#include <stdint.h>
#include <sys/cdefs.h>
#define MAKE_TAG_CONSTANT(A,B,C,D) (((A) << 24) | ((B) << 16) | ((C) << 8) | (D))
#define HARDWARE_MODULE_TAG MAKE_TAG_CONSTANT('H','W','M','T')
#define HARDWARE_HAL_API_VERSION HARDWARE_MAKE_API_VERSION(1,0)
#define HARDWARE_MAKE_API_VERSION(maj,min) ((((maj) & 0xff) << 8) | ((min) & 0xff))
struct hw_module_t {
    uint32_t tag;
    uint16_t module_api_version;
    uint16_t hal_api_version;
    const char *id;
    const char *name;
    const char *author;
    void *methods;
    void *dso;
    uint8_t reserved[32 - 7*sizeof(void*)];
};
struct hw_device_t {
    uint32_t tag;
    uint32_t version;
    struct hw_module_t *module;
    uint8_t reserved[12];
    int (*close)(struct hw_device_t *device);
};
int hw_get_module(const char *id, const struct hw_module_t **module);
#endif
H
    fi
    if [ ! -f include/cutils/native_handle.h ]; then
        cat > include/cutils/native_handle.h << 'H'
#ifndef NATIVE_HANDLE_H_
#define NATIVE_HANDLE_H_
#include <stdint.h>
typedef struct native_handle { int version; int numFds; int numInts; int data[0]; } native_handle_t;
#endif
H
    fi

    # Fix hooks.c: add missing fortify declarations
    if ! grep -q 'extern int __vsprintf_chk' common/hooks.c; then
        sed -i '/Wrap some GCC builtin/i\
extern int __vsprintf_chk(char *s, int flag, size_t slen, const char *format, __builtin_va_list ap);\
extern int __vsnprintf_chk(char *s, size_t n, int flag, size_t slen, const char *format, __builtin_va_list ap);' common/hooks.c
    fi

    # Configure and build
    CC=clang CXX=clang++ ./autogen.sh \
        --enable-wayland --enable-arch=arm64 \
        --with-android-headers="$LIBHYBRIS_DIR/hybris/include" \
        --prefix=/usr 2>&1 | tail -3

    # Build just common + properties (all we need for android_dlopen)
    make -j4 \
        CXXFLAGS="-Wno-non-pod-varargs" \
        CFLAGS="-Wno-deprecated-declarations" \
        -C common 2>&1 | tail -5
    make -C common install 2>&1 | tail -3
    make -C properties 2>&1 | tail -3
    make -C properties install 2>&1 | tail -3
    make -C include install 2>&1 | tail -3

    ldconfig 2>/dev/null || true
    echo "[hybris-icd] libhybris installed"
fi

# ── Build the Vulkan ICD ────────────────────────────────────────────
echo "[hybris-icd] Building Vulkan ICD..."
clang -shared -fPIC -fno-stack-protector \
    -o "$ICD_SO" \
    "$SRC_DIR/vulkan_hybris_icd.c" \
    -I/usr/include \
    -L/usr/lib -lhybris-common \
    -Wl,-rpath,/usr/lib

# Install ICD manifest
mkdir -p "$(dirname "$ICD_JSON")"
cp "$SRC_DIR/hybris_vulkan_icd.json" "$ICD_JSON"

echo "[hybris-icd] Installed: $ICD_SO"
echo "[hybris-icd] Installed: $ICD_JSON"

# Quick smoke test
echo "[hybris-icd] Running smoke test..."
VK_ICD_FILENAMES="$ICD_JSON" timeout 10 vulkaninfo --summary 2>&1 | grep -E "GPU|deviceName|Physical" || echo "[hybris-icd] Smoke test: no GPU found (may need /vendor bind)"
