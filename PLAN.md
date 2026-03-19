# Implementation Plan

## Milestone 0: Compositor boots on device [DONE]

- [x] Project scaffolding: Cargo.toml, module structure, edition 2024
- [x] Port smithay compositor from localdesktop (compositor, state, delegates)
- [x] Port EGL/GLES backend (winit window, EGLDisplay, GlesRenderer)
- [x] Port input backend (WinitInput types, keymap, event centralizer)
- [x] Port event handler (rendering loop, input dispatch)
- [x] Port proot process/launch stubs
- [x] Port ApplicationContext (JNI data_dir, native_library_dir)
- [x] Cross-compile with cargo-ndk for aarch64-linux-android
- [x] Gradle project for APK packaging (NativeActivity manifest)
- [x] Bundle prebuilt libs (libxkbcommon.so, libproot.so, libproot_loader.so)
- [x] Fix Wayland socket bind (create dirs, remove stale socket)
- [x] Fix xkbcommon (set XKB_CONFIG_ROOT, push xkb data to device)
- [x] App runs on device: compositor renders dark red background at 2160x1584

## Milestone 1: Single Wayland client rendering [DONE]

Goal: `weston-terminal` (or `foot`) renders in the single NativeActivity window.

### 1a. proot + Arch ARM filesystem

- [x] Download Arch Linux ARM rootfs tarball (proot-distro v4.34.2)
- [x] Extract to `{data_dir}/arch/` on device
- [x] Create fake /proc files for proot compatibility
- [x] Verify proot can execute a simple command (`/bin/ls`)
- [x] Install wayland client packages in arch: `pacman -S weston`

### 1b. Wayland socket connectivity

- [x] Verify socket path is accessible from proot environment
- [x] Socket at `{ARCH_FS_ROOT}/tmp/wayland-0` accessible from proot
- [x] Client inside proot connects to compositor socket
- [x] Compositor accepts client, creates wl_surface

### 1c. SHM buffer rendering

- [x] Compositor receives wl_shm buffer from client
- [x] Upload SHM buffer data as GLES texture
- [x] Render texture to the EGL surface (full window)
- [x] Verify: weston-terminal visible on screen

### 1d. Basic input [DONE]

- [x] Forward touch events as pointer events to the Wayland client
- [x] Forward key events (hardware keyboard) to the client
- [x] Test: can type in weston-terminal
- Note: Input was already fully ported from localdesktop (event_centralizer.rs + event_handler.rs)

### 1e. Use xkb data from Arch rootfs [DONE]

- [x] Point XKB_CONFIG_ROOT at `{ARCH_FS_ROOT}/usr/share/X11/xkb`
- [x] Fix absolute symlink to relative (so it resolves outside proot)
- [x] Remove manual `adb push` workaround

## Milestone 2: Multi-window (Activity-per-toplevel) [DONE]

Goal: each XDG toplevel gets its own Android Activity.

### 2a. Java Activity class [DONE]

- [x] WaylandWindowActivity class with SurfaceView
- [x] Activity receives window_id via Intent extras
- [x] SurfaceHolder callbacks → JNI (nativeSurfaceCreated/Changed/Destroyed)
- [x] Touch and key events forwarded via JNI
- [x] FLAG_ACTIVITY_NEW_DOCUMENT | FLAG_ACTIVITY_MULTIPLE_TASK for separate recents

### 2b. Compositor window management [DONE]

- [x] WindowManager maps toplevel → window_id → Activity
- [x] Launch Activity via JNI (using app classloader, not system classloader)
- [x] Create separate EGLSurface per Activity via ANativeWindow
- [x] Render each client's buffer to its own Activity's EGL surface
- [x] xdg_toplevel destroy → finish the corresponding Activity

### 2c. Display/output per window [DONE]

- [x] SurfaceChanged → configure toplevel with Activity dimensions
- [x] Handle Activity lifecycle (isFinishing vs config change)

## Milestone 3: Input routing [DONE]

Goal: each Activity correctly routes input to its Wayland client.

- [x] Each Activity forwards touch/key events via JNI with window_id
- [x] Compositor dispatches to correct toplevel based on window_id
- [x] Touch → pointer motion/button (ACTION_DOWN/UP/MOVE)
- [x] Key events → keyboard input (Android keycode → Linux keycode)
- [x] Keyboard focus set on touch-down per window
- [x] Soft keyboard: auto show/hide via zwp_text_input_v3 protocol (GTK/Qt apps)
- [ ] Soft keyboard: manual toggle for apps that don't use text_input (e.g. terminals)
- [ ] Handle focus: Android focus changes map to Wayland keyboard enter/leave
- [ ] Multi-touch passthrough (currently single-touch only)

## Milestone 4: Resize [MOSTLY DONE]

Goal: Activity resize triggers proper Wayland configure round-trip.

- [x] SurfaceChanged callback → compositor gets new dimensions
- [x] Compositor sends xdg_toplevel configure(width, height) to client
- [x] Client acks configure, submits buffer at new size
- [x] Compositor re-renders at new size
- [x] Works in DeX freeform window mode
- [ ] Handle split-screen transitions gracefully

## Milestone 5: Drop winit from main Activity [MOSTLY DONE]

Goal: the main Activity no longer needs NativeActivity or winit.

- [x] Create headless EGL context (no window surface needed)
- [x] Replace winit event loop with background thread + libc::poll()
- [x] Drive rendering from damage (Wayland commits, window events) not vsync
- [x] Remove render_main_window() (eliminates busy-loop GPU waste)
- [x] Convert MainActivity from NativeActivity to plain Activity
- [x] Status overlay shows directly in Activity layout (no EGL surface to fight)

## Milestone 6: Zero-copy GPU rendering [MOSTLY DONE]

Goal: eliminate CPU copy in the rendering path.

### Vulkan clients (DONE — zero-copy)
- [x] Vulkan renderer (`src/android/backend/vulkan_renderer.rs`) using `ash` crate
- [x] Import client dmabufs via proprietary Qualcomm Vulkan (`DMA_BUF_BIT_EXT`)
- [x] `vkCmdCopyBufferToImage` with explicit stride to swapchain
- [x] `vkQueuePresent` to Android SurfaceView
- [x] Cache imported dmabufs by fd (zero per-frame allocations)
- [x] Lazy Vulkan swapchain creation (only on first dmabuf commit)
- [x] Fallback to GLES/mmap for wl_shm clients (gedit, GTK apps)

### OpenGL clients via Zink (IN PROGRESS)
- [x] Mesa patch: EGL Wayland fallback to kopper when GBM unavailable
- [x] Mesa patch: Adreno 830 chip_id wildcard (`0x440500ff`)
- [x] Mesa patch: `dri2_setup_device` software EGLDevice when no DRM fd
- [x] GPU rendering confirmed: glmark2 score 1415 off-screen (1416fps)
- [x] EGL Wayland init now works with Zink/Kopper on KGSL
- [x] Vulkan WSI creates dmabuf buffers via `zwp_linux_buffer_params`
- [x] Compositor destroys EGL surface before Vulkan swapchain takeover
- [ ] Fix GPU fault: Zink causes KGSL GUILTY context reset on first render
      Root cause: `vkQueueSubmit` → KGSL ioctl → GPU fault → EDEADLK.
      Happens with ALL GL ops but NOT with direct Vulkan (vkcube works).
      Off-screen: GPU recovers (800-1400fps). On-screen: KGSL context killed.
      Likely Zink generates an unsupported command for Adreno 830.
      Need: upstream Mesa investigation or newer Mesa version.

### Failed approaches (documented in GPU_RENDERING.md)
- [x] EGL dmabuf import — extension not available on Android
- [x] AHardwareBuffer from dmabuf fd — Samsung gralloc rejects
- [x] GL_EXT_memory_object_fd — `glTexStorageMem2DEXT` broken on Qualcomm
- [x] Vulkan bridge (dmabuf→opaque fd→GL) — GL still rejects
- [x] Vulkan layer — `VK_ANDROID_external_memory_android_hardware_buffer` compiled out

## Milestone 7: Polish

- [ ] Window titles: xdg_toplevel title → Activity label (visible in recents)
- [ ] App icons: extract client app icon → Activity icon
- [ ] Clipboard: bridge Wayland clipboard (wl_data_device) ↔ Android clipboard
- [ ] Lifecycle: handle Android app suspend/resume gracefully
- [ ] Error handling: graceful error messages instead of panics
- [ ] Config UI: username, launch command, check/install commands (currently hardcoded constants in config.rs)
- [ ] Configurable HiDPI scale (default taken from android but changeable)
- [ ] Main UI shows setup status including whether all necessary permissions are granted.
- [ ] hideable apps in app launcher are configurable - long press shows menu with hide buton and there's a button to unhide all.

- [ ] SOUND SUPPORT. pipewire.
- [ ] eliminate all C use because C is dirty

## Milestone 8: NixOS

As an alternative to Arch in proot, we want to also allow NixOS WITHOUT proot. NixOS will just run with a different prefix than `/nix` - since it already has patches to allow running in an isolated environment (all files in other dirs than a program expects) for every single package it should give us lots of things for free.

We do not really care about the purity aspects of NixOS though, so we should do the setup in a way where comfort is more important than beauty. (e.g. no immutable home dir)

## Recent Fixes

- [x] Automated first-run setup (download rootfs, install weston via pacman)
- [x] Setup progress overlay (SetupOverlay.java, WindowManager panel)
- [x] bwrap shim for glycin/gdk-pixbuf (proot can't do namespaces)
- [x] xdg-decoration: send_pending_configure in new_decoration (fixes CSD in gedit)
- [x] Activity lifecycle: isFinishing check prevents fullscreen toggle killing apps
- [x] process::exit(0) after event loop (prevents ndk-context double-init panic)
- [x] keepDebugSymbols in Gradle (readable stack traces)
- [x] Status overlay via JNI (client/toplevel info on main activity)
- [x] MANAGE_EXTERNAL_STORAGE permission: prompt on launch via Settings redirect
- [x] CAMERA permission: requested on launch (grants /dev/video* access in proot via camera group)
- [x] Bind /storage/emulated/0 and /sdcard in proot (conditional on permission being granted)
- [x] setup_storage_mountpoints(): create bind destination dirs in rootfs during setup
- [x] Launcher icon fixes: SVG support (androidsvg library), search AdwaitaLegacy/legacy subdir
- [x] proot launch fix: restore probe-based seccomp (PROOT_NO_SECCOMP=1 breaks exec in app context), drop runuser in favour of direct sh (setuid fails without seccomp virtualization)

## Known Issues

- Main activity EGL surface renders over status overlay (needs Milestone 5)
- libxkbcommon.so has hardcoded path from localdesktop; overridden via XKB_CONFIG_ROOT
- Single-touch only (no multi-touch passthrough yet)
- No Wayland keyboard enter/leave on Activity focus changes
- PipeWire camera crashes (SIGBUS in protocol-native module) — disabled for now
- OpenGL apps via Zink: GPU fault (KGSL GUILTY reset) on first Zink render kills KGSL context
  - `vkQueueSubmit` fails with EDEADLK after GPU reset — not recoverable on-screen
  - Off-screen: GPU recovers, runs at 800-1400fps; on-screen: KGSL context permanently dead
  - Direct Vulkan (vkcube) unaffected — issue is Zink-specific command generation
  - Needs upstream Mesa fix or newer Mesa version with Adreno 830 Zink fixes
- Mesa must be built with gcc, not clang (clang produces Turnip that doesn't recognize Adreno 830)
- Mesa build in proot has intermittent `posix_spawn` failures with `-j4`
