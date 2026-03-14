# GPU Rendering on Android via Turnip/KGSL

## Overview

Linux apps running in proot can use hardware GPU acceleration via **Turnip** (Mesa's open-source Vulkan driver for Adreno GPUs) talking to the **KGSL** kernel driver (`/dev/kgsl-3d0`). The compositor imports rendered frames via `zwp_linux_dmabuf_v1`.

**Current status**: Working with mmap fallback (CPU copy for compositing). GPU does the actual 3D rendering.

## Device Info

- Samsung SM8750 (Snapdragon 8 Elite), Adreno 830 GPU
- chip_id=0x44050001, 12MB GMEM
- Android 16, API 35, kernel 6.x
- KGSL at `/dev/kgsl-3d0` (Qualcomm proprietary, not standard DRM)

## Architecture

```
┌──────────────────────┐    ┌───────────────────────────────┐
│  Linux App (proot)   │    │  Compositor (Android app)     │
│                      │    │                               │
│  Vulkan API          │    │  smithay (Wayland server)     │
│  ↓                   │    │  ↓                            │
│  Turnip (Mesa)       │    │  GlesRenderer (Android EGL)   │
│  ↓                   │    │  ↓                            │
│  KGSL ioctls         │    │  Android SurfaceView          │
│  ↓                   │    │                               │
│  /dev/kgsl-3d0       │    │                               │
└──────────┬───────────┘    └───────────────┬───────────────┘
           │                                │
           │  zwp_linux_dmabuf_v1           │
           │  (dmabuf fd over unix socket)  │
           └────────────────────────────────┘
```

## Components

### 1. KGSL Shim (`kgsl_shim/kgsl_shim.c`) — debugging only, NOT required

Samsung's kernel omits `IOCTL_KGSL_VERSION` (ioctl nr=0x03). Turnip (Mesa 26.0.1) handles
this gracefully — the shim is **not needed** for Turnip to initialize. It was useful during
development for logging KGSL ioctls and confirming GPU communication.

**Build** (inside proot, optional):
```sh
clang -shared -fPIC -fuse-ld=lld -o /usr/local/lib/kgsl_shim.so kgsl_shim.c -ldl
```

Use with `LD_PRELOAD=/usr/local/lib/kgsl_shim.so` to log all KGSL ioctls for debugging.

### 2. Dmabuf Support in Compositor

Android EGL **lacks** `EGL_EXT_image_dma_buf_import` (confirmed in logcat). Changes made:

- **`src/android/compositor/mod.rs`**: Added `DmabufState`, `DmabufGlobal`, `DmabufHandler`, `delegate_dmabuf!`
- **`src/android/app.rs`**: Queries renderer for dmabuf formats; falls back to `FormatSet::from_formats_hardcoded()` (ARGB/XRGB/ABGR/XBGR8888 + LINEAR modifier)
- **Smithay patch** (`patches/smithay/`):
  - `format.rs`: Added `FormatSet::from_formats_hardcoded()`
  - `display.rs`: Relaxed EGL extension check — attempts `eglCreateImageKHR(EGL_LINUX_DMA_BUF_EXT)` even without the extension string
  - `gles/mod.rs`: Added `import_dmabuf_via_mmap()` fallback — when EGLImage creation fails, mmaps the dmabuf fd and uploads pixels via `glTexSubImage2D`

### 3. Rendering Pipeline (current — mmap fallback)

```
1. Turnip renders scene on GPU via KGSL
2. Exports frame as dmabuf fd
3. Sends to compositor via zwp_linux_dmabuf_v1 Wayland protocol
4. Compositor receives dmabuf fd
5. Tries eglCreateImageKHR(EGL_LINUX_DMA_BUF_EXT) → FAILS (no extension)
6. Fallback: mmap(dmabuf_fd) → glTexSubImage2D → GL texture
7. Compositor renders texture onto Android EGL surface
```

The 3D rendering (step 1) is GPU-accelerated. The compositing step (6) involves a CPU roundtrip.

## How to Reproduce (vkcube test)

### Prerequisites (one-time, inside proot)
```sh
# Install Turnip and Vulkan tools
./adb_runas.sh pacman -S vulkan-freedreno vulkan-tools

# Build the KGSL shim
./adb_runas.sh <<'EOF'
cd /path/to/kgsl_shim
clang -shared -fPIC -fuse-ld=lld -o /usr/local/lib/kgsl_shim.so kgsl_shim.c -ldl
EOF
```

### Running vkcube
```sh
# 1. Start the compositor app
./run.sh

# 2. In another terminal, launch vkcube inside proot
./adb_runas.sh vkcube
```

The proot launch (`src/android/proot/process.rs`) automatically sets `WAYLAND_DISPLAY=wayland-0`
and `XDG_RUNTIME_DIR=/tmp`. Turnip's ICD is found via the default Vulkan loader search path.

### Verifying GPU acceleration
```sh
./adb_runas.sh vulkaninfo --summary
# Expected: "Turnip Adreno (TM) 830", vendorID=0x5143, driverID=DRIVER_ID_MESA_TURNIP
```

### Common issues

| Issue | Cause | Fix |
|-------|-------|-----|
| `Cannot connect to wayland` | Wrong XDG_RUNTIME_DIR | Use `XDG_RUNTIME_DIR=/tmp` (not `/data/local/tmp`) |
| Black window | No dmabuf support in compositor | Need the dmabuf + mmap fallback patches |
| vkcube hangs after "Selected GPU" | No dmabuf global advertised | Compositor must advertise `zwp_linux_dmabuf_v1` |

## EGL Extensions Available on Device

**Has:**
- `EGL_KHR_image_base` (can create EGLImages)
- `EGL_ANDROID_image_native_buffer` (import AHardwareBuffer as EGLImage)
- `EGL_ANDROID_get_native_client_buffer` (AHardwareBuffer → EGLClientBuffer)
- `EGL_ANDROID_native_fence_sync`
- `GL_EXT_memory_object` + `GL_EXT_memory_object_fd` (import external memory via fd)
- `GL_OES_EGL_image` (bind EGLImage to GL texture)

**Lacks:**
- `EGL_EXT_image_dma_buf_import` (the standard Linux dmabuf import — NOT available)
- `EGL_EXT_image_dma_buf_import_modifiers`
- `EGL_MESA_image_dma_buf_export`

## Zero-Copy Path: AHardwareBuffer (TODO)

The current mmap fallback involves a CPU copy. For zero-copy, we need:

```
dmabuf fd → AHardwareBuffer → eglGetNativeClientBufferANDROID → eglCreateImageKHR → GL texture
```

### Approach A: dlsym `AHardwareBuffer_createFromHandle` (private API)

```c
// Private VNDK function in libnativewindow.so
int AHardwareBuffer_createFromHandle(
    const AHardwareBuffer_Desc* desc,
    const native_handle_t* handle,
    int32_t method,  // 0=REGISTER, 1=CLONE
    AHardwareBuffer** outBuffer
);
```

- `dlopen("libnativewindow.so")` + `dlsym("AHardwareBuffer_createFromHandle")`
- Construct `native_handle_t` with dmabuf fd, fill `AHardwareBuffer_Desc` with width/height/format/stride
- Risk: private API, may break across Android versions, Samsung gralloc may need extra metadata

### Approach B: Compositor-allocated AHardwareBuffers (wlroots-android-bridge approach)

- Compositor allocates AHardwareBuffers via `AHardwareBuffer_allocate()` (public NDK API)
- Extracts dmabuf fd from AHB (via socketpair trick or gralloc handle)
- Shares these buffers with clients via `zwp_linux_dmabuf_v1`
- Client renders into compositor-provided buffers
- See `refs/wlroots-android-bridge/` — they use `cros_gralloc_handle.h` to extract buffer attributes

**Key difference from our approach**: wlroots-android-bridge also uses `ASurfaceTransaction_setBuffer` to present directly to SurfaceFlinger, bypassing EGL compositing entirely. This is the most efficient path but requires significant architecture changes.

### Approach C: Vulkan interop

- Import dmabuf fd into Vulkan via `VkImportMemoryFdInfoKHR`
- Export as AHardwareBuffer via `vkGetMemoryAndroidHardwareBufferANDROID`
- Use AHardwareBuffer with EGL
- Most complex, requires Vulkan instance in compositor

### EGL import once you have AHardwareBuffer

```c
// Step 1: AHB → EGLClientBuffer
EGLClientBuffer clientBuf = eglGetNativeClientBufferANDROID(ahb);

// Step 2: EGLClientBuffer → EGLImage
EGLint attrs[] = { EGL_IMAGE_PRESERVED_KHR, EGL_TRUE, EGL_NONE };
EGLImageKHR image = eglCreateImageKHR(
    display, EGL_NO_CONTEXT,
    EGL_NATIVE_BUFFER_ANDROID,  // 0x3140
    clientBuf, attrs);

// Step 3: EGLImage → GL texture
glBindTexture(GL_TEXTURE_2D, tex);
glEGLImageTargetTexture2DOES(GL_TEXTURE_2D, image);
```

## References

- `refs/wlroots-android-bridge/` — wlroots compositor on Android with AHardwareBuffer allocator
- [AHardwareBuffer NDK docs](https://developer.android.com/ndk/reference/group/a-hardware-buffer)
- [EGL_ANDROID_get_native_client_buffer spec](https://registry.khronos.org/EGL/extensions/ANDROID/EGL_ANDROID_get_native_client_buffer.txt)
- [EGL_ANDROID_image_native_buffer spec](https://registry.khronos.org/EGL/extensions/ANDROID/EGL_ANDROID_image_native_buffer.txt)
- Spencer Fricke's AHardwareBuffer shared memory article (medium.com)
- [Turnip KGSL backend](https://gitlab.freedesktop.org/mesa/mesa/-/tree/main/src/freedreno/vulkan)
