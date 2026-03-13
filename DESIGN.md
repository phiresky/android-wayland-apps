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
- Each Activity owns an EGL surface backed by Android's stock GLES driver
- The compositor renders the Wayland client's buffer onto this surface
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

**Why stock Android GLES instead of mesa?**
Mesa's open-source GPU drivers (iris, freedreno) have limited Android support
and would restrict us to specific hardware. Android's stock GLES drivers work
on all ARM devices. The compositor uses EGL/GLES via Android's libEGL.so
directly.

## Wayland ↔ Android Feature Mapping

| Wayland Feature | Android Mapping | Why needed | Status |
|---|---|---|---|
| **Shell surfaces** | | | |
| xdg_toplevel | Each toplevel → own Activity, appears as separate task in recents | Core protocol — every app window is a toplevel | Implemented |
| xdg_popup | Rendered as subsurface of parent toplevel's Activity | Menus, tooltips, dropdowns (e.g. GTK right-click menus) | Implemented |
| wlr_layer_shell | Each layer surface → own Activity (same as toplevel) | Desktop shell components: panels, app launchers, notification areas (e.g. nwg-panel, waybar). Apps crash without it. | Implemented |
| **Rendering** | | | |
| wl_shm buffers | Compositor uploads SHM → GLES texture → Activity EGL surface | Baseline rendering path — all Wayland clients support this | Implemented |
| dmabuf / AHardwareBuffer | Zero-copy GPU via ASurfaceTransaction | Eliminates CPU copy for GPU-rendered clients (GTK4 GL, games) | Not implemented |
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

### Phase 1: SHM (CPU copy, initial implementation)
- Wayland clients render into shared memory buffers (wl_shm)
- Compositor reads the SHM buffer and uploads it as a GLES texture
- Texture is drawn to the Activity's EGL surface
- Simple, works everywhere, but involves a CPU copy

### Phase 2: Zero-copy GPU (optimization)
- Use AHardwareBuffer as the backing store for Wayland buffers
- Compositor creates AHardwareBuffers via NDK and shares them with clients
  as dmabuf file descriptors
- Client renders directly into the GPU buffer
- Buffer is presented to the Activity's surface via ASurfaceTransaction
- Zero CPU copies in the rendering path
- Requires implementing a custom smithay allocator backed by AHardwareBuffer

## Input Path

Android Activity receives touch/keyboard/mouse events via winit or raw
NativeActivity callbacks. The compositor translates these to Wayland input
events and dispatches them to the focused client via wl_seat.

For multi-window, each Activity forwards its input events to the compositor,
which knows which Wayland client owns that Activity's surface.

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
