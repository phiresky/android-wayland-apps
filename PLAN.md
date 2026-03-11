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

### 1d. Basic input

- [ ] Forward touch events as pointer events to the Wayland client
- [ ] Forward key events (hardware keyboard) to the client
- [ ] Test: can type in weston-terminal

### 1e. Bundle xkb data properly

- [ ] Include xkb data as Android assets or in the APK
- [ ] Copy xkb data to app files dir on first launch
- [ ] Remove manual `adb push` workaround

## Milestone 2: Multi-window (Activity-per-toplevel)

Goal: each XDG toplevel gets its own Android Activity.

### 2a. Java Activity class

- [ ] Create a Java/Kotlin WaylandWindowActivity class
- [ ] Activity receives a window ID via Intent extras
- [ ] Activity creates a SurfaceView and passes the Surface to native code via JNI
- [ ] Register JNI functions for native code to call back (create activity, destroy activity)

### 2b. Compositor window management

- [ ] When xdg_toplevel is created, call JNI to launch new WaylandWindowActivity
- [ ] Map each toplevel to an Activity (window_id → surface mapping)
- [ ] Create a separate EGL surface for each Activity's window
- [ ] Render each client's buffer to its own Activity's EGL surface
- [ ] When xdg_toplevel is destroyed, finish the corresponding Activity

### 2c. Display/output per window

- [ ] Each Activity reports its size to the compositor
- [ ] Compositor sends xdg_toplevel configure with the Activity's dimensions
- [ ] Handle Activity lifecycle: pause/resume/destroy

## Milestone 3: Input routing

Goal: each Activity correctly routes input to its Wayland client.

- [ ] Each Activity forwards touch/key events via JNI to compositor
- [ ] Compositor identifies which client owns the Activity and dispatches events
- [ ] Handle focus: Android focus changes map to Wayland keyboard enter/leave
- [ ] Handle pointer motion, button press, scroll
- [ ] Handle touch (multi-touch passthrough)
- [ ] Handle keyboard with proper keymap

## Milestone 4: Resize

Goal: Activity resize triggers proper Wayland configure round-trip.

- [ ] Activity resize callback → compositor gets new dimensions
- [ ] Compositor sends xdg_toplevel configure(width, height) to client
- [ ] Client acks configure, submits buffer at new size
- [ ] Compositor resizes EGL surface and renders new buffer
- [ ] Handle split-screen, freeform window mode

## Milestone 5: Zero-copy GPU rendering

Goal: eliminate CPU copy in the rendering path.

- [ ] Create AHardwareBuffer-backed allocator for smithay
- [ ] Share AHardwareBuffers with clients as dmabuf file descriptors
- [ ] Client renders directly into GPU buffer (for clients that support linux-dmabuf)
- [ ] Present buffer via ASurfaceTransaction (or EGLImage)
- [ ] Fallback to SHM path for clients that don't support dmabuf
- [ ] Benchmark: measure latency and throughput improvement

## Milestone 6: Polish

- [ ] Window titles: xdg_toplevel title → Activity label (visible in recents)
- [ ] App icons: extract client app icon → Activity icon
- [ ] Clipboard: bridge Wayland clipboard (wl_data_device) ↔ Android clipboard
- [ ] Lifecycle: handle Android app suspend/resume gracefully
- [ ] First-run setup: download and extract Arch rootfs automatically
- [ ] Error handling: graceful error messages instead of panics
- [ ] Build automation: single command builds cargo + gradle + installs APK

## Build & Deploy

```bash
# Build native library
source .env
cargo ndk -t arm64-v8a --platform 35 build --release

# Build APK
cd android && ./gradlew assembleDebug

# Install and run
adb install -r android/build/outputs/apk/debug/wayland-launcher-debug.apk
adb shell am start -n io.github.phiresky.wayland_android/android.app.NativeActivity

# View logs
adb logcat | grep -E "android_wayland_launc|RustStdoutStderr|RustPanic|smithay"
```

## Known Issues

- xkb data must be manually pushed to device (workaround until bundled in APK)
- libxkbcommon.so has hardcoded path from localdesktop (app.polarbear); overridden via XKB_CONFIG_ROOT env var
- proot launch silently fails when Arch filesystem is not installed
- Lots of trace-level smithay logging (should reduce to info/warn for production)
