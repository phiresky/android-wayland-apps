# GPU Rendering on Android via Turnip/KGSL

## Overview

Linux apps running in proot can use hardware GPU acceleration via **Turnip** (Mesa's open-source Vulkan driver for Adreno GPUs) talking to the **KGSL** kernel driver (`/dev/kgsl-3d0`). The compositor imports rendered frames via `zwp_linux_dmabuf_v1`.

**Current status**: Zero-copy GPU compositing via Vulkan bridge. Both client rendering
and compositor display use the same KGSL GPU memory — no CPU copies.

## Device Info

- Samsung SM8750 (Snapdragon 8 Elite), Adreno 830 GPU
- chip_id=0x44050001, 12MB GMEM
- Android 16, API 35, kernel 6.x
- KGSL at `/dev/kgsl-3d0` (Qualcomm proprietary, not standard DRM)

## Architecture

Two rendering paths depending on client buffer type:

### Vulkan clients (zero-copy) — dmabuf path
```
┌──────────────────────┐    ┌───────────────────────────────┐
│  Linux App (proot)   │    │  Compositor (Android app)     │
│                      │    │                               │
│  Vulkan API          │    │  VulkanRenderer (ash crate)   │
│  ↓                   │    │  ↓ import DMA_BUF_BIT_EXT     │
│  Turnip (Mesa)       │    │  ↓ vkCmdCopyBufferToImage     │
│  ↓                   │    │  ↓ vkQueuePresent             │
│  KGSL ioctls         │    │  Android SurfaceView          │
│  ↓                   │    │                               │
│  /dev/kgsl-3d0  ←────────→  Qualcomm proprietary Vulkan   │
└──────────┬───────────┘    └───────────────┬───────────────┘
           │  same KGSL GPU memory (zero-copy)              │
           │  zwp_linux_dmabuf_v1 (fd passing)              │
           └────────────────────────────────────────────────┘
```

Key insight: both Turnip (Mesa) and the proprietary Qualcomm Vulkan driver talk to
the same KGSL kernel driver. The proprietary driver accepts Turnip's dmabufs via
`VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT` (works even though the extension
`VK_EXT_external_memory_dma_buf` is not advertised).

### wl_shm clients (CPU) — fallback path
```
┌──────────────────────┐    ┌───────────────────────────────┐
│  Linux App (proot)   │    │  Compositor (Android app)     │
│                      │    │                               │
│  Software rendering  │    │  smithay GlesRenderer         │
│  ↓                   │    │  ↓ (standard wl_shm import)   │
│  wl_shm buffer       │    │  ↓ glTexSubImage2D            │
│                      │    │  Android EGL surface          │
└──────────┬───────────┘    └───────────────┬───────────────┘
           │  wl_shm (shared memory)        │
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

### 2. Vulkan Renderer (`src/android/backend/vulkan_renderer.rs`)

The compositor uses the **proprietary Qualcomm Vulkan driver** (NOT Turnip) for
zero-copy dmabuf compositing. This is separate from smithay's GLES renderer.

- Creates a Vulkan instance + device using Android's `libvulkan.so`
- Extensions: `VK_KHR_swapchain`, `VK_KHR_external_memory_fd`, `VK_KHR_android_surface`
- Per-window: creates `VkSwapchainKHR` on the Android `ANativeWindow`
- Per-frame: imports client dmabuf via `DMA_BUF_BIT_EXT`, creates `VkBuffer` with
  explicit stride, `vkCmdCopyBufferToImage` to swapchain, `vkQueuePresent`
- Caches imported dmabufs by fd (~3-5 swapchain buffers, reused across frames)

### 3. Dmabuf Protocol Support

- **`src/android/compositor/mod.rs`**: `DmabufState`, `DmabufGlobal`, `DmabufHandler`
- Advertises hardcoded formats (ARGB/XRGB/ABGR/XBGR8888 + LINEAR)
- Accepts all dmabufs optimistically in `dmabuf_imported`

### 4. GLES Fallback (smithay patch)

For wl_shm clients or if Vulkan renderer is unavailable:
- `import_dmabuf_via_mmap()`: mmaps dmabuf fd, uploads via `glTexSubImage2D`
- `import_dmabuf_via_ahb()`: AHardwareBuffer path (fails on Samsung gralloc)
- `import_dmabuf_via_memory_object()`: GL_EXT_memory_object_fd (broken on Qualcomm)

### 5. Rendering Pipeline (zero-copy Vulkan path)

```
1. Turnip renders scene on GPU via KGSL
2. Exports frame as dmabuf fd (KGSL GPU memory)
3. Sends fd to compositor via zwp_linux_dmabuf_v1
4. Compositor receives fd, looks up in dmabuf cache (by fd, validated by size)
5. If not cached: vkAllocateMemory(DMA_BUF import) + vkCreateBuffer
6. vkCmdCopyBufferToImage from buffer to LINEAR staging VkImage (BGRA format)
7. vkCmdBlitImage from staging to swapchain image (BGRA→RGBA conversion)
8. vkQueuePresent to Android SurfaceView
```

Steps 1-8 are all GPU operations — zero CPU copies. The staging image in step 6-7
is needed because:
- Android's Vulkan swapchain only supports R8G8B8A8, but Turnip/Zink dmabufs use
  DRM_FORMAT_XRGB8888 (B8G8R8A8 memory layout). `vkCmdBlitImage` handles the
  format conversion.
- The imported dmabuf VkImage has OPTIMAL tiling, which Qualcomm interprets as UBWC
  (Universal Bandwidth Compression). Since the dmabuf is LINEAR, reading it via the
  imported VkImage directly causes horizontal stripes. The LINEAR staging image avoids this.
- `ANativeWindow_setBuffersGeometry(format=BGRA)` does NOT work — the Qualcomm
  Vulkan driver always creates R8G8B8A8 swapchains regardless of the ANativeWindow format.

### 6. Key Implementation Details

- **Dmabuf cache validation**: Cached by fd, but validated by width/height on lookup.
  Kernel fd reuse means the same fd number can point to different GPU memory after
  resize. Stale cache entries caused a strobe between current and frozen frames.
- **Lazy EGL surface creation**: EGL surfaces are NOT created in `surfaceCreated`.
  Creating EGL eagerly locks the ANativeWindow format to RGBA, which the Vulkan
  swapchain inherits. Deferring to the first GLES render lets the Vulkan path claim
  the window first (for dmabuf clients). For wl_shm clients, EGL is created lazily
  when the GLES render path first needs it.
- **Swapchain resize**: Uses `oldSwapchain` chaining (not destroy+recreate) to avoid
  `ERROR_NATIVE_WINDOW_IN_USE_KHR` races. Buffer geometry is set via
  `ANativeWindow_setBuffersGeometry` to match the client's buffer size — Android's
  SurfaceFlinger upscales to fill the SurfaceView.
- **Frame callback for unmapped windows**: Frame callbacks are sent for windows without
  EGL/VK surfaces. Without this, EGL clients (e.g. Factorio via llvmpipe) block
  forever in `eglSwapBuffers` waiting for a callback that never comes.
- **needs_redraw guard**: Vulkan blits only run on new commits. Re-blitting without
  new content causes a race condition — the compositor reads the dmabuf while the
  client writes the next frame to the same GPU memory (fd reuse in swapchain pool).

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

### Client compatibility

| Client type | Status | Path |
|-------------|--------|------|
| Vulkan (vkcube) | **Working** | Turnip → dmabuf → VkBuffer import → staging blit → swapchain |
| OpenGL (via Zink) | **Working** | Zink → Turnip → Kopper → dmabuf → same Vulkan path |
| OpenGL (box64, e.g. Factorio) | **Working** | x86_64 Zink via box64 → dmabuf → Vulkan path |
| wl_shm (software) | **Working** | CPU shared memory → GLES renderer (lazy EGL surface) |

### OpenGL via Zink

OpenGL apps use **Zink** (Mesa's OpenGL-over-Vulkan layer) with **Kopper** (Zink's WSI).
The Vulkan WSI creates `zwp_linux_buffer_params` with dmabuf fds (LINEAR modifier),
which the compositor imports via the same Vulkan path as native Vulkan clients.

**Tested apps:**
- eglgears_wayland — GPU-accelerated gears via Zink+Turnip
- Factorio 2.0 via box64 — full GPU rendering, 1102 MB VRAM, OpenGL 4.6
- glmark2 — 1415 score at 60fps

**Env vars for Zink:** `MESA_LOADER_DRIVER_OVERRIDE=zink GALLIUM_DRIVER=zink`

### Building Mesa from source

Mesa main (26.1.0-devel+) has correct Adreno 830 support (`chip_id=0x44050001`,
KGSL backend). Three small patches are needed:

1. **UBWC 5.0**: Samsung A830 reports UBWC version 5, not handled in Mesa main.
   Add `KGSL_UBWC_5_0` constant and case (same config as UBWC 4.0).
2. **KHR_display**: KGSL backend rejects instances with `VK_KHR_display` enabled,
   but the Vulkan loader enables it. Remove the check.
3. **EGL Wayland fallback** (for Zink/OpenGL only): Fall back to kopper/swrast path
   when DRM/GBM unavailable, and use software EGLDevice when no render node fd.

**Build deps** (install as root in proot):
```sh
pacman -S --needed meson ninja gcc cmake python-mako python-packaging python-yaml \
  libdrm libxshmfence wayland wayland-protocols pkgconf libxrandr libelf llvm clang \
  lm_sensors libglvnd vulkan-icd-loader glslang bison flex binutils
```

**Configure** (Turnip only — for Vulkan clients):
```sh
cd ~/mesa
CC=gcc CXX=g++ meson setup builddir \
  -Dgallium-drivers= -Dvulkan-drivers=freedreno -Dplatforms=wayland \
  -Dglx=disabled -Degl=disabled -Dgles1=disabled -Dgles2=disabled \
  -Dopengl=false -Dbuildtype=release -Dfreedreno-kmds=kgsl,msm --prefix=/usr
```

**Configure** (full stack — for Zink/OpenGL + Vulkan):
```sh
CC=gcc CXX=g++ meson setup builddir \
  -Dgallium-drivers=zink -Dvulkan-drivers=freedreno -Dplatforms=wayland \
  -Dglx=disabled -Degl=enabled -Dgles2=enabled -Dgles1=disabled \
  -Dopengl=true -Dbuildtype=release -Dfreedreno-kmds=kgsl,msm --prefix=/usr
```

**Build and install:**
```sh
ninja -C builddir -j4
sudo ninja -C builddir install  # or: USER_NAME=root ./adb_runas.sh ...
```

**Important notes:**
- `-Dfreedreno-kmds=kgsl,msm` (not just `kgsl`). With only `kgsl`, Mesa disables
  libdrm, but the Wayland WSI needs it for `WSI_IMAGE_TYPE_DRM` dmabuf swapchains.
- Must use gcc, not clang (clang produces Turnip that doesn't recognize Adreno 830).

### Legacy: Mesa 26.0.x patches

Two patches in `patches/` were needed for Mesa 26.0.1 (no longer needed on main):

1. **`mesa-zink-wayland-fallback.patch`**: EGL Wayland fallback (still needed on main
   for Zink, applied directly to source).
2. **`mesa-adreno830-chipid.patch`**: Backport real Adreno 830 GPU config.
   Mesa main already has the correct config with `chip_id=0x44050001`.

### Common issues

| Issue | Cause | Fix |
|-------|-------|-----|
| `Cannot connect to wayland` | Wrong XDG_RUNTIME_DIR | Use `XDG_RUNTIME_DIR=/tmp` (not `/data/local/tmp`) |
| `EGLUT: failed to initialize EGL display` | No DRM render node in proot | Use `MESA_LOADER_DRIVER_OVERRIDE=zink GALLIUM_DRIVER=zink` with patched Mesa |
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

## Zero-Copy: Vulkan Compositor (WORKING)

The proprietary Qualcomm Vulkan driver imports Turnip's KGSL dmabufs directly via
`VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT` — even though the extension
`VK_EXT_external_memory_dma_buf` is NOT advertised. Both drivers share the same
KGSL kernel driver, so the GPU memory is never copied.

```
dmabuf fd (from KGSL/Turnip)
  → vkAllocateMemory(DMA_BUF_BIT_EXT)           ✓ zero-copy import
  → vkCreateBuffer (explicit stride)             ✓ raw buffer view
  → vkCmdCopyBufferToImage (to swapchain)        ✓ GPU-side blit
  → vkQueuePresent (Android surface)             ✓ displayed
```

**Key discovery:** The GL path (EGL, GL_EXT_memory_object_fd) is completely broken
on Qualcomm Adreno 830. The working path bypasses GLES entirely — compositor uses
the proprietary Vulkan driver directly for dmabuf import and swapchain presentation.

The GLES renderer (smithay GlesRenderer) is still used for wl_shm clients. Both
renderers coexist: each window has both an EGL surface and a Vulkan swapchain,
and the compositor picks the right path based on buffer type.

## All Approaches Tried

Before finding the working Vulkan import path, we tried every standard zero-copy
approach on the Samsung SM8750 (Snapdragon 8 Elite). All failed except #10.

### Summary

| # | Approach | Result |
|---|----------|--------|
| 1 | EGL dmabuf import | **No extension** — `EGL_EXT_image_dma_buf_import` not available on Android |
| 2 | Relax EGL extension check | **EGL_NO_IMAGE_KHR** — `eglCreateImageKHR(EGL_LINUX_DMA_BUF_EXT)` returns null |
| 3 | AHardwareBuffer from dmabuf fd | **EINVAL** — Samsung gralloc rejects bare KGSL native_handle |
| 4 | GL_EXT_memory_object_fd (DMA_BUF) | **GL_INVALID_VALUE** — `glTexStorageMem2DEXT` always fails |
| 5 | GL_EXT_memory_object_fd (OPAQUE) | **GL_INVALID_VALUE** — same failure with opaque fd handle type |
| 6 | Vulkan bridge (dmabuf → opaque fd → GL) | **GL_INVALID_VALUE** — GL still rejects the memory object |
| 7 | TEXTURE_TILING_EXT hint | **SIGSEGV** — crashes Qualcomm driver |
| 8 | Vulkan layer (`VK_ANDROID_external_memory_android_hardware_buffer`) | **Not feasible** — compiled out in Turnip Linux build |
| 9 | wlroots-android-bridge style (minigbm/gralloc) | **Not attempted** — too invasive |
| **10** | **Proprietary Qualcomm Vulkan + `DMA_BUF_BIT_EXT`** | **WORKING — zero-copy** |

### #1–2: EGL dmabuf import

Android EGL completely lacks `EGL_EXT_image_dma_buf_import`. Even bypassing the
extension check and calling `eglCreateImageKHR(EGL_LINUX_DMA_BUF_EXT)` directly
returns `EGL_NO_IMAGE_KHR`. The EGL implementation simply doesn't support dmabuf
as an image source.

### #3: AHardwareBuffer from dmabuf fd (EINVAL)

Used private VNDK function `AHardwareBuffer_createFromHandle` from `libnativewindow.so`
(symbols load fine via dlsym). Constructs `native_handle_t` with dmabuf fd +
`AHardwareBuffer_Desc` with width/height/format/stride. Samsung gralloc rejects with
EINVAL because the `native_handle_t` lacks vendor-specific metadata fields (Samsung
gralloc handles contain ~20 extra ints for tile mode, internal format, buffer ID, etc.).
Would likely work on AOSP/Pixel devices with a more permissive gralloc.

### #4–5: GL_EXT_memory_object_fd (GL_INVALID_VALUE)

Device has `GL_EXT_memory_object` + `GL_EXT_memory_object_fd` extensions.
`glImportMemoryFdEXT(mem, size, handle_type, fd)` succeeds (no GL error) for both
`GL_HANDLE_TYPE_DMA_BUF_FD_EXT` and `GL_HANDLE_TYPE_OPAQUE_FD_EXT`. But
`glTexStorageMem2DEXT(GL_TEXTURE_2D, 1, GL_RGBA8, w, h, mem, 0)` always returns
`GL_INVALID_VALUE`. Tried: actual width, stride-based width, page-aligned size.
Conclusion: Qualcomm's implementation is non-functional for external memory import.

### #6: Vulkan bridge (dmabuf → opaque fd → GL)

Attempted to convert KGSL dmabuf fds to Vulkan opaque fds using the proprietary
Qualcomm Vulkan driver (import via `DMA_BUF_BIT_EXT`, export via `OPAQUE_FD_BIT`),
then import the opaque fd into GL via `GL_EXT_memory_object_fd`. The Vulkan import/export
works, but GL still rejects the memory object with `GL_INVALID_VALUE` (same as #4–5).

### #7: TEXTURE_TILING_EXT hint (SIGSEGV)

Attempted `glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_TILING_EXT, GL_LINEAR_TILING_EXT)`
to hint that the dmabuf memory is linearly tiled. Crashes the Qualcomm driver immediately
— `GL_TEXTURE_TILING_EXT` is not a valid pname on this hardware.

### #8: Vulkan layer intercepting swapchain

Would require intercepting `vkCreateSwapchainKHR` and replacing buffer allocation with
AHardwareBuffer-backed memory. `VK_ANDROID_external_memory_android_hardware_buffer` is
gated behind `#if DETECT_OS_ANDROID` in Turnip source. Mesa inside proot is a Linux build,
so the extension is compiled out. Would essentially require reimplementing the Vulkan WSI.

### #9: wlroots-android-bridge style (minigbm/gralloc allocator)

Not attempted. Would require building minigbm (Google's GBM implementation) for ARM64,
configuring Mesa to use GBM with gralloc backend instead of raw KGSL, making both client
and server allocations AHB-backed. This is the approach used by `refs/wlroots-android-bridge/`
— they use `cros_gralloc_handle.h` from minigbm and present via `ASurfaceTransaction_setBuffer`.
Key insight: they don't import bare dmabufs at all — the entire stack uses AHBs from the start.

### #10: Proprietary Qualcomm Vulkan + DMA_BUF_BIT_EXT (WORKING)

The proprietary Qualcomm Vulkan driver accepts Turnip's KGSL dmabufs via
`VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT` — even though `VK_EXT_external_memory_dma_buf`
is NOT advertised. Both drivers share the same KGSL kernel driver, so the GPU memory
is never copied. See "Zero-Copy: Vulkan Compositor" section above for details.

### EGL import path (reference — works with valid AHardwareBuffer)

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

This path works (EGL bindings loaded, functions resolved). The bottleneck is
step 0: getting a valid AHardwareBuffer from a KGSL dmabuf fd (blocked by #3).

## References

- `refs/wlroots-android-bridge/` — wlroots compositor on Android with AHardwareBuffer allocator
- [AHardwareBuffer NDK docs](https://developer.android.com/ndk/reference/group/a-hardware-buffer)
- [EGL_ANDROID_get_native_client_buffer spec](https://registry.khronos.org/EGL/extensions/ANDROID/EGL_ANDROID_get_native_client_buffer.txt)
- [EGL_ANDROID_image_native_buffer spec](https://registry.khronos.org/EGL/extensions/ANDROID/EGL_ANDROID_image_native_buffer.txt)
- Spencer Fricke's AHardwareBuffer shared memory article (medium.com)
- [Turnip KGSL backend](https://gitlab.freedesktop.org/mesa/mesa/-/tree/main/src/freedreno/vulkan)
