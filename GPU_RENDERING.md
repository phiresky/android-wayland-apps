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

## Zero-Copy: Vulkan Bridge (PROMISING — in progress)

The proprietary Qualcomm Vulkan driver can bridge raw KGSL dmabufs into opaque fds
that the proprietary GL driver should accept. Confirmed via standalone C test:

```
dmabuf fd (from KGSL/Turnip)
  → vkAllocateMemory(VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT)     ✓ works
  → vkGetMemoryFdKHR(VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT_KHR)   ✓ works, returns valid fd
  → glImportMemoryFdEXT(GL_HANDLE_TYPE_OPAQUE_FD_EXT)                    ? untested with opaque fd
  → glTexStorageMem2DEXT                                                  ? untested
  → GL texture (zero-copy!)
```

**Key findings from testing:**
- Proprietary Qualcomm Vulkan driver (Adreno 830) supports `VK_KHR_external_memory_fd` ✓
- `VK_EXT_external_memory_dma_buf` is NOT advertised, but `DMA_BUF_BIT_EXT` handle type
  **works anyway** — `vkGetMemoryFdPropertiesKHR` returns `memoryTypeBits=0x12` ✓
- DMA_BUF import into VkDeviceMemory → success ✓
- Export as OPAQUE_FD from imported memory → success (fd returned) ✓
- Combined alloc (DMA_BUF import + OPAQUE_FD export flags) → success ✓
- Export as AHardwareBuffer → FAILS (`VK_ERROR_OUT_OF_HOST_MEMORY`) ✗
  (cross-export between DMA_BUF and AHB handle types not supported)

**Implementation plan:**
1. Compositor creates a Vulkan instance + device (proprietary Qualcomm driver) at startup
2. When a dmabuf arrives from a client, import it via `VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT`
3. Export as `VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT_KHR` → opaque fd
4. Import opaque fd into GL via `GL_EXT_memory_object_fd` with `GL_HANDLE_TYPE_OPAQUE_FD_EXT`
5. `glTexStorageMem2DEXT` → GL texture for compositing

The Vulkan driver acts as a translator between raw KGSL dmabufs and the proprietary
Qualcomm GL driver's opaque fd format. Both drivers use KGSL under the hood, so the
actual GPU memory is never copied.

**Test binary:** `vk_import_test2.c` in project root (cross-compile with Android NDK).

## Previous Zero-Copy Attempts (all failed on Samsung/Qualcomm)

The current mmap fallback involves a CPU copy per frame. We tried every available
direct zero-copy path on the Samsung SM8750 (Snapdragon 8 Elite). All failed.

### Results summary

| Approach | Result | Details |
|----------|--------|---------|
| EGL dmabuf import | **No extension** | Android EGL lacks `EGL_EXT_image_dma_buf_import` entirely |
| Relax EGL check | **EGL_NO_IMAGE_KHR** | `eglCreateImageKHR(EGL_LINUX_DMA_BUF_EXT)` returns null even without the extension check |
| AHardwareBuffer from dmabuf fd | **EINVAL (-22)** | Samsung gralloc rejects bare KGSL native_handle — needs vendor-specific metadata ints |
| GL_EXT_memory_object_fd (DMA_BUF) | **GL_INVALID_VALUE** | `glImportMemoryFdEXT` succeeds but `glTexStorageMem2DEXT` fails — driver only supports Vulkan opaque fd interop |
| GL_EXT_memory_object_fd (OPAQUE) | **GL_INVALID_VALUE** | Same — opaque fd import appears to succeed but texture binding fails |
| TEXTURE_TILING_EXT hint | **SIGSEGV** | `glTexParameteri(TEXTURE_TILING_EXT)` crashes Qualcomm driver — not a valid pname |
| Vulkan layer approach | **Not feasible** | `VK_ANDROID_external_memory_android_hardware_buffer` is compiled out in Turnip's Linux build (proot) |

### Approach A: AHardwareBuffer_createFromHandle — FAILED (EINVAL)

**Implemented in:** `patches/smithay/src/backend/renderer/gles/mod.rs` (`import_dmabuf_via_ahb`)

- Private VNDK function in `libnativewindow.so` — symbols load fine via dlsym
- Constructs `native_handle_t` with dmabuf fd + `AHardwareBuffer_Desc` with width/height/format/stride
- Samsung gralloc rejects with EINVAL because the `native_handle_t` lacks vendor-specific metadata fields (Samsung gralloc handles contain ~20 extra ints for tile mode, internal format, buffer ID, etc.)
- **Would work on AOSP/Pixel** devices with a more permissive gralloc

### Approach B: GL_EXT_memory_object_fd — FAILED (GL_INVALID_VALUE)

**Implemented in:** `patches/smithay/src/backend/renderer/gles/mod.rs` (`import_dmabuf_via_memory_object`, disabled)

- Device has `GL_EXT_memory_object` + `GL_EXT_memory_object_fd` extensions
- `glImportMemoryFdEXT(mem, size, GL_HANDLE_TYPE_DMA_BUF_FD_EXT, fd)` succeeds (no GL error)
- `glTexStorageMem2DEXT(GL_TEXTURE_2D, 1, GL_RGBA8, w, h, mem, 0)` always returns `GL_INVALID_VALUE`
- Tried: actual width, stride-based width, page-aligned size, LINEAR tiling hint
- Conclusion: Qualcomm's GL_EXT_memory_object_fd only supports Vulkan-exported opaque fds, not dmabuf fds

### Approach C: Vulkan layer intercepting swapchain — NOT FEASIBLE

- Would require intercepting `vkCreateSwapchainKHR` and replacing buffer allocation
- `VK_ANDROID_external_memory_android_hardware_buffer` is gated behind `#if DETECT_OS_ANDROID` in Turnip source
- Mesa inside proot is a Linux build → extension is compiled out
- Essentially requires reimplementing the entire Vulkan WSI — too complex

### Approach D: wlroots-android-bridge style (minigbm/gralloc allocator)

**Not attempted — would require:**
- Building minigbm (Google's GBM implementation) for ARM64
- Configuring Mesa to use GBM with gralloc backend instead of raw KGSL
- Both client and server allocations become AHB-backed
- This is the only proven path to zero-copy on Android (used by wlroots-android-bridge)
- See `refs/wlroots-android-bridge/` — they use `cros_gralloc_handle.h` from minigbm

**Key insight from wlroots-android-bridge**: they don't try to import bare dmabufs. They
make the ENTIRE allocation stack use AHBs from the start (client GBM → gralloc → AHB).
The compositor then presents via `ASurfaceTransaction_setBuffer` directly to SurfaceFlinger.

### EGL import path (works once you have a valid AHardwareBuffer)

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

This path is confirmed to work (EGL bindings loaded, functions resolved). The
bottleneck is always step 0: getting a valid AHardwareBuffer from a KGSL dmabuf fd.

## References

- `refs/wlroots-android-bridge/` — wlroots compositor on Android with AHardwareBuffer allocator
- [AHardwareBuffer NDK docs](https://developer.android.com/ndk/reference/group/a-hardware-buffer)
- [EGL_ANDROID_get_native_client_buffer spec](https://registry.khronos.org/EGL/extensions/ANDROID/EGL_ANDROID_get_native_client_buffer.txt)
- [EGL_ANDROID_image_native_buffer spec](https://registry.khronos.org/EGL/extensions/ANDROID/EGL_ANDROID_image_native_buffer.txt)
- Spencer Fricke's AHardwareBuffer shared memory article (medium.com)
- [Turnip KGSL backend](https://gitlab.freedesktop.org/mesa/mesa/-/tree/main/src/freedreno/vulkan)
