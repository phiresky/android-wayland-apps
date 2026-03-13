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

## Milestone 5: Drop winit from main Activity

Goal: the main Activity no longer needs NativeActivity or winit.

Currently the main NativeActivity hosts the winit event loop which:
1. Drives the compositor (accept clients, dispatch protocol, render)
2. Creates the EGL context (shared by all window surfaces)
3. Renders a useless dark background every frame (busy loop at 60fps)

Each WaylandWindowActivity already has its own EGL surface. The shared EGL context
can be created headless (PBuffer surface) without needing a window.

- [ ] Create headless EGL context (PBuffer) instead of window-backed
- [ ] Replace winit event loop with a simple loop on a background thread
- [ ] Convert MainActivity from NativeActivity to plain Activity
- [x] Remove render_main_window() (eliminates busy-loop GPU waste)
- [x] Drive rendering from damage (Wayland commits, window events) not vsync polling
  - ControlFlow::Wait instead of Poll
  - EventLoopProxy::wake_up() from JNI callbacks, Wayland commits, and socket watcher
  - Background thread polls listener + display fds with libc::poll()
- [ ] Status overlay shows directly in Activity layout (no EGL surface to fight)

## Milestone 6: Zero-copy GPU rendering

Goal: eliminate CPU copy in the rendering path.

- [ ] Create AHardwareBuffer-backed allocator for smithay
- [ ] Share AHardwareBuffers with clients as dmabuf file descriptors
- [ ] Client renders directly into GPU buffer (for clients that support linux-dmabuf)
- [ ] Present buffer via ASurfaceTransaction (or EGLImage)
- [ ] Fallback to SHM path for clients that don't support dmabuf
- [ ] Benchmark: measure latency and throughput improvement

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

 ## Recent Fixes

- [x] Automated first-run setup (download rootfs, install weston via pacman)
- [x] Setup progress overlay (SetupOverlay.java, WindowManager panel)
- [x] bwrap shim for glycin/gdk-pixbuf (proot can't do namespaces)
- [x] xdg-decoration: send_pending_configure in new_decoration (fixes CSD in gedit)
- [x] Activity lifecycle: isFinishing check prevents fullscreen toggle killing apps
- [x] process::exit(0) after event loop (prevents ndk-context double-init panic)
- [x] keepDebugSymbols in Gradle (readable stack traces)
- [x] Status overlay via JNI (client/toplevel info on main activity)

## Known Issues

- Main activity EGL surface renders over status overlay (needs Milestone 5)
- libxkbcommon.so has hardcoded path from localdesktop; overridden via XKB_CONFIG_ROOT
- Single-touch only (no multi-touch passthrough yet)
- No Wayland keyboard enter/leave on Activity focus changes
