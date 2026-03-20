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
4. Compositor receives fd, looks up in dmabuf cache (by fd)
5. If not cached: vkAllocateMemory(DMA_BUF import) + vkCreateBuffer
6. vkCmdCopyBufferToImage from imported buffer to swapchain image
7. vkQueuePresent to Android SurfaceView
```

Steps 1-7 are all GPU operations — zero CPU copies. The "copy" in step 6 is a
GPU-side blit between two KGSL memory regions on the same hardware.

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
| Vulkan (vkcube, games) | **Zero-copy, working** | Turnip → dmabuf → Vulkan import → swapchain |
| OpenGL (via Zink) | **Working** | Zink → Turnip → Kopper → Vulkan WSI → dmabuf → Vulkan import → swapchain |
| wl_shm (software) | **Working** | CPU shared memory → GLES renderer |

### OpenGL via Zink

OpenGL apps use **Zink** (Mesa's OpenGL-over-Vulkan layer) with **Kopper** (Zink's WSI).
The Vulkan WSI creates `zwp_linux_buffer_params` with dmabuf fds (LINEAR modifier),
which the compositor imports via the same Vulkan zero-copy path as native Vulkan clients.

**Benchmarks:** glmark2 scores 1415 on-screen at 60fps via
`MESA_LOADER_DRIVER_OVERRIDE=zink GALLIUM_DRIVER=zink`.

**Two Mesa patches** fix the EGL → Kopper → Vulkan WSI → dmabuf pipeline:
1. `mesa-zink-wayland-fallback.patch`: Fall back to kopper/swrast path when DRM/GBM
   unavailable, and use software EGLDevice when no render node fd exists.
2. `mesa-adreno830-chipid.patch`: Backport real Adreno 830 GPU config from Mesa main —
   correct `reg_size_vec4` (96→128), `tile_align_w` (64→96), `a8xx_gen2` properties,
   and raw magic registers. The stock Mesa 26.0.1 config was "totally fake" and caused
   GPU faults (KGSL GUILTY reset).

The compositor destroys the EGL surface before creating the Vulkan swapchain on the
same ANativeWindow (Android doesn't allow both).

### Mesa patches required

Two patches in `patches/` for Mesa 26.0.1:

1. **`mesa-zink-wayland-fallback.patch`**: Two changes to `platform_wayland.c`:
   - Fall back to kopper/swrast path when DRM/GBM unavailable
   - `dri2_setup_device` passes `software=true` when `fd_render_gpu < 0`
   Without these, `eglInitialize` fails on Wayland (Mesa assumes DRM always available,
   and `dri2_setup_device` asserts on missing render node fd).

2. **`mesa-adreno830-chipid.patch`**: Backport real Adreno 830 GPU config from Mesa main.
   Samsung's chip reports `0x44050001` but Mesa 26.0.1 only matches `0x44050000` —
   the patch wildcards the revision byte (`0x440500ff`). More critically, it replaces
   the "totally fake" GPU config with correct values: `a8xx_gen2` properties,
   `reg_size_vec4=128`, `tile_align_w=96`, `a8xx_gen2_raw_magic_regs`. Without this,
   Turnip triggers KGSL GUILTY resets (GPU faults) during rendering.

**Build:** Must use gcc (not clang — clang produces Turnip that doesn't recognize
the GPU). Build in proot with `ninja -j4`.

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
