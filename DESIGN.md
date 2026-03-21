# android-wayland-launcher

Launch native Linux Wayland applications as individual Android activities.

## Overview

A Rust-based Wayland compositor runs in-process within an Android app. Linux
applications run inside a proot Arch Linux ARM environment and connect to the
compositor via a Wayland socket. Each XDG toplevel window is presented as its
own Android Activity, so the Android window manager handles tiling, stacking,
alt-tab, and split-screen natively.

Example target app: gedit.

## Architecture

```
┌─────────────────────────────────────────────────┐
│  Android App Process (Rust, NDK)                │
│                                                 │
│  ┌───────────────┐                              │
│  │   smithay     │◄── Wayland socket ──┐       │
│  │  compositor   │                      │       │
│  └───┬───┬───┬───┘                      │       │
│      │   │   │                          │       │
│  ┌───v┐┌─v─┐┌v───┐                ┌─────┴─────┐ │
│  │Act ││Act││Act │                │  proot    │ │
│  │ 1  ││ 2 ││ 3  │                │  Arch ARM │ │
│  │    ││   ││    │                │           │ │
│  │GLES││   ││    │                │ gedit     │ │
│  │surf││   ││    │                │ gtk4      │ │
│  └────┘└───┘└────┘                │ ...       │ │
│                                   └───────────┘ │
└─────────────────────────────────────────────────┘
```

### Components

**Wayland compositor (smithay, in-process)**
- Handles the Wayland protocol: wl_compositor, xdg_shell, wl_shm, wl_seat
- Listens on a Unix socket that proot apps connect to
- When a client creates an XDG toplevel, the compositor spawns a new Android
  Activity via JNI and routes the window's buffer to that Activity's surface
- Forwards Android input events back to the appropriate Wayland client

**Android Activities (one per window)**
- Each Activity has a SurfaceView; the compositor creates an ASurfaceControl
  child and presents frames via ASurfaceTransaction_setBuffer
- GPU-rendered clients (dmabuf): Vulkan imports the dmabuf, blits to an
  AHardwareBuffer target, presents via ASurfaceTransaction (zero CPU copy)
- Software clients (wl_shm): GLES texture upload to an EGL surface (fallback)
- Android's window manager handles positioning, resizing, and lifecycle
- On resize, the compositor sends xdg_toplevel configure events back to the
  client

**Linux environment (proot + Arch Linux ARM)**
- Arch Linux ARM filesystem stored in the app's internal storage
- proot provides a rootless chroot-like environment via ptrace syscall
  interception
- No root required
- Standard packages install via pacman (gtk4, gedit, etc.)
- Apps connect to the compositor's Wayland socket (exposed via
  `WAYLAND_DISPLAY` env var)

## Key Design Decisions

**Why smithay (Rust) instead of wlroots (C)?**
The compositor runs in-process as part of the Android app. Rust integrates
cleanly with Android NDK via JNI, and smithay gives us a Wayland compositor
library without needing to bridge C code running in a separate Termux process.
localdesktop proves this approach works on real ARM devices.

**Why Activity-per-window instead of single fullscreen?**
Android's window manager already handles tiling, stacking, split-screen, and
task switching. By mapping each Linux window to an Activity, we get all of this
for free instead of reimplementing it in the compositor.

**Why proot + Arch instead of Termux native?**
Termux recompiles packages against Android's Bionic libc with non-standard
paths ($PREFIX). Desktop Linux apps (GTK, gedit) assume standard paths
(/usr/lib, /usr/share) and are painful to port. proot intercepts syscalls to
provide a standard Linux filesystem layout where `pacman -S gedit` just works.

**Why not chroot/LXC?**
Requires root on Android. proot is rootless.

**Why proprietary Qualcomm Vulkan + AHB instead of GLES/swapchain?**
Mesa's GL_EXT_memory_object_fd is broken on Qualcomm Adreno 830. The working
path uses the proprietary Vulkan driver to import client dmabufs via
DMA_BUF_BIT_EXT (unadvertised but functional — both Turnip and the proprietary
driver share the same KGSL kernel driver). Presentation uses AHardwareBuffer +
ASurfaceTransaction instead of a Vulkan swapchain, enabling hardware overlays
and explicit vsync control. GLES/EGL is only used as fallback for wl_shm clients.

## Wayland ↔ Android Feature Mapping

| Wayland Feature | Android Mapping | Why needed | Status |
|---|---|---|---|
| **Shell surfaces** | | | |
| xdg_toplevel | Each toplevel → own Activity, appears as separate task in recents | Core protocol — every app window is a toplevel | Implemented |
| xdg_popup | Rendered as subsurface of parent toplevel's Activity | Menus, tooltips, dropdowns (e.g. GTK right-click menus) | Implemented |
| wlr_layer_shell | Each layer surface → own Activity (same as toplevel) | Desktop shell components: panels, app launchers, notification areas (e.g. nwg-panel, waybar). Apps crash without it. | Implemented |
| **Rendering** | | | |
| wl_shm buffers | Compositor uploads SHM → GLES texture → Activity EGL surface | Baseline rendering path — all Wayland clients support this | Implemented |
| dmabuf / AHardwareBuffer | Vulkan import → GPU blit → AHB → ASurfaceTransaction | Zero CPU copy for GPU-rendered clients (GTK4 GL, games) | Implemented |
| wp_fractional_scale | Activity display density (scale factor from DisplayMetrics) | HiDPI: apps render at native resolution instead of being scaled | Implemented |
| Surface damage | Full-surface redraw each frame | Partial damage tracking would reduce GPU work | Implemented (no partial) |
| **Input** | | | |
| wl_pointer | Activity onTouchEvent → pointer motion/button via JNI channel | Primary interaction method; touch emulated as pointer clicks | Implemented |
| wl_keyboard | Activity onKeyEvent → keycode translation → wl_keyboard | Physical keyboards, DeX mode, Bluetooth keyboards | Implemented |
| wl_touch | Not exposed; touch events emulated as pointer | Multi-touch gestures (pinch-zoom, two-finger scroll) | Not implemented |
| zwp_text_input_v3 | Android soft keyboard (IME) show/hide per-Activity | On-screen keyboard for text input in apps like terminals, editors | Implemented |
| Cursor image | Not rendered (touch-first UI) | Desktop apps set custom cursors; matters for DeX mouse mode | Not implemented |
| **Window management** | | | |
| Toplevel configure (resize) | Activity resize → xdg_configure round-trip | Apps must know their size to layout content correctly | Implemented |
| Toplevel close | Activity finish ↔ xdg_toplevel send_close | Closing window in Android recents should close the Linux app | Implemented |
| Window title | Could map to Activity label | Shows app name in recents/taskbar instead of generic "Wayland" | Not implemented |
| Window minimize/maximize | Android handles via Activity lifecycle / DeX | Android WM provides these controls natively | Delegated to Android |
| Split-screen / freeform | Android WM positions Activities; compositor follows | DeX and tablet split-screen work because each window is an Activity | Works naturally |
| Alt-tab / recents | Each Activity is a separate task | | Works naturally |
| **Data** | | | |
| wl_data_device (clipboard) | Protocol handler exists, no Android ClipboardManager bridge | Copy/paste between Linux apps and Android apps | Partial |
| Drag and drop | wl_data_device DnD handlers | Drag files/text between windows | Not implemented |
| **Decorations** | | | |
| xdg_decoration | Forces server-side; Android provides window chrome in DeX mode | Prevents CSD (client-side decorations) which would conflict with Android's own title bars | Implemented |
| **Lifecycle** | | | |
| Compositor process | Foreground service (CompositorService) keeps process alive | Without this, Android kills the compositor when Activities are backgrounded | Implemented |
| Activity destroy (config change) | Surface destroyed/recreated; toplevel kept alive | Screen rotation, fold/unfold — Android destroys and recreates the Activity | Implemented |
| Activity finish (user close) | send_close to Wayland client | | Implemented |

## Rendering Path

### wl_shm path (software clients)
- Wayland clients render into shared memory buffers (wl_shm)
- Compositor uploads SHM buffer as GLES texture via smithay GlesRenderer
- Texture drawn to the Activity's EGL surface
- Works everywhere, involves CPU copy

### dmabuf path (GPU clients — current)
- Client renders via Turnip (Mesa Vulkan for Adreno) → exports dmabuf fd
- Compositor imports dmabuf via proprietary Qualcomm Vulkan (DMA_BUF_BIT_EXT)
- GPU blit: dmabuf → LINEAR staging image → AHardwareBuffer (BGRA→RGBA)
- Presents via ASurfaceTransaction_setBuffer — bypasses Vulkan swapchain
- OnComplete callback for vsync-locked frame pacing
- Async GPU fence (VK_KHR_external_fence_fd) → sync fd passed to SurfaceFlinger
- Zero CPU copies; one GPU blit for format conversion

### Phase 3: True zero-copy (in progress)
- Compositor allocates AHardwareBuffers via custom smithay allocator (AhbAllocator)
- Exports AHB as dmabuf fd (via AHardwareBuffer_getNativeHandle)
- Client renders directly into the compositor's AHB
- Compositor presents the AHB directly — no import, no staging, no GPU blit
- AhbBufferTracker uses inode matching to recognize compositor-allocated dmabufs
- Requires wiring server-side allocation into zwp_linux_dmabuf_v1 protocol

## Input Path

Each WaylandWindowActivity forwards touch/keyboard/mouse events to the
compositor thread via JNI callbacks → mpsc channel → eventfd wake.
The compositor translates these to Wayland input events and dispatches
them to the focused client via wl_seat.

## References

- [wlroots-android-bridge](https://github.com/Xtr126/wlroots-android-bridge)
  (cloned in ./refs) - Demonstrates Activity-per-window and zero-copy GPU
  buffer path using ASurfaceTransaction. Uses wlroots/labwc in C with
  Kotlin Android app. x86_64 + Intel mesa only.

- [localdesktop](https://github.com/localdesktop/localdesktop) (cloned in
  ./refs) - Rust Wayland compositor using smithay + winit on Android. Uses
  proot + Arch ARM. Single fullscreen window with Xwayland. Proves the
  smithay-on-Android approach works on real ARM phones.

## Future Work

**Nix as Linux environment (replacing proot + Arch)**

proot has inherent overhead from ptrace syscall interception. A potential
optimization is replacing the proot + Arch environment with Nix using a
custom store path.

Nix packages are self-contained closures with all dependencies resolved to
absolute paths (e.g. `/nix/store/<hash>-gtk4-4.14/lib/libgtk-4.so`). By
compiling Nix with a custom store path inside the app's data directory
(e.g. `/data/data/com.app/files/nix/store/`), binaries get the correct
interpreter and library paths baked in and run directly on the kernel with
no ptrace interception and no path translation.

Tradeoffs:
- Pro: Native execution speed (no proot overhead)
- Pro: Reproducible, declarative environments (flake.nix defines exactly
  what's installed)
- Pro: Self-contained closures are easy to bundle or download as a unit
- Con: Custom store path invalidates the standard nixpkgs binary cache
  (all hashes change), requiring a build farm or CI pipeline to produce
  ARM64 binaries with the custom prefix
- Con: Alternatively, use `patchelf` to rewrite interpreter paths in
  pre-built binaries from the standard cache (fragile but avoids the
  build farm)

The Linux environment layer is intentionally decoupled from the compositor,
so this swap can happen independently once the compositor architecture is
stable.

**Client-side GPU acceleration**

glibc applications running in proot cannot directly load Android's
Bionic-linked GPU drivers. To enable client-side GPU rendering (e.g. GTK4
GL renderer, Vulkan apps), a translation layer like virgl (OpenGL) or
venus (Vulkan) would be needed to proxy GPU commands from the client to
the compositor process which has access to Android's GPU. This is a
significant undertaking but would unlock hardware-accelerated rendering
for Linux apps.

## Milestones

1. **Single window**: Strip localdesktop down to a minimal smithay compositor
   that displays one native Wayland app (e.g. `weston-terminal`) in a single
   Activity via proot + Arch
2. **Multi-window**: When a new XDG toplevel is created, spawn a new Android
   Activity via JNI and route the buffer there
3. **Input routing**: Forward each Activity's input events to the correct
   Wayland client
4. **Resize**: Handle Activity resize → xdg_toplevel configure round-trip
5. **Zero-copy GPU**: Implement AHardwareBuffer-backed allocator in smithay
6. **Polish**: Window titles, app icons, clipboard, proper lifecycle handling
