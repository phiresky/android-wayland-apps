# Project: android-wayland-apps

Rust-based Wayland compositor on Android. Linux apps run in proot+Arch ARM, render via smithay compositor to Android NativeActivity EGL surface.

## Build

```sh
source .env
cargo ndk build          # debug build (default for dev)
cd android && ./gradlew installDebug
adb shell am start -n io.github.phiresky.wayland_android/io.github.phiresky.wayland_android.MainActivity
```

- `cargo ndk` args (`-t arm64-v8a --platform 35`) are set in `.env` via `CARGO_NDK_TARGET`/`CARGO_NDK_PLATFORM`
- Gradle copies .so from `target/aarch64-linux-android/debug/` (see `assembleNativeLibs` in build.gradle.kts)
- Use debug builds for dev iteration, not `--release`

## Device

```sh
# Take screenshot (Samsung fold, display ID required):
adb exec-out screencap -p -d 4630947200649055635 > screenshot.png
```

## Arch Rootfs Setup (automated)

Setup is fully automated via `src/android/proot/setup.rs`:
- On first launch, downloads rootfs tarball, extracts, sets up DNS, installs weston via pacman
- Progress shown in a UI overlay (SetupOverlay.java) on top of the EGL surface
- `is_setup_complete()` checks both rootfs existence AND `pacman -Q weston`
- `fix_xkb_symlink()` runs at every startup (converts absolute symlink to relative)

Source: https://github.com/termux/proot-distro/releases/latest (aarch64 archlinux tarball)
Installed to: `/data/data/io.github.phiresky.wayland_android/files/arch` (= `ARCH_FS_ROOT`)

WARNING: `pm clear` wipes the rootfs! Use `am force-stop` instead.

### XKB data

- Lives in rootfs at `usr/share/X11/xkb` (symlink to `usr/share/xkeyboard-config-2/`)
- The symlink is absolute in the tarball; `fix_xkb_symlink()` converts it to relative at startup
- Prebuilt libxkbcommon.so has hardcoded path from app.polarbear — overridden via `XKB_CONFIG_ROOT` env var
- Compositor will SIGSEGV if XKB data is missing; `init_keyboard()` guards with path check
- Package: `xkeyboard-config` from Arch repos

### Running proot from adb shell

A helper script can be pushed to the device - see `run_proot.sh` pattern in the codebase.
The app's proot setup is in `src/android/proot/process.rs` (ArchProcess).

## Architecture

- `MainActivity` extends `NativeActivity` (for splash screen handling, `reportFullyDrawn()`)
- Setup overlay uses `WindowManager.addView(TYPE_APPLICATION_PANEL)` to draw on top of native EGL surface
- Overlay must be shown from `can_create_surfaces` callback (window token not available earlier)
- Setup runs in background thread; event loop starts immediately to dismiss Android 12+ splash screen
- Keyboard init deferred until xkb data directory exists (prevents SIGSEGV)
- Don't use immersive fullscreen on WaylandWindowActivity — breaks in DeX windowed mode

## Key Config

- Package: `io.github.phiresky.wayland_android`
- Edition: 2024 (requires `unsafe(no_mangle)`, unsafe blocks in unsafe fns)
- Target SDK: 35, min SDK: 34
- NDK: 29.0.14206865
- Prebuilt libs (libs/arm64-v8a/): libxkbcommon.so, libproot.so, libproot_loader.so (from localdesktop - TODO: build ourselves)
- Gradle: AGP 9.1.0, Gradle 9.3.1, configuration caching enabled
- Clippy: `deny(clippy::unwrap_used, clippy::expect_used)` — no bare unwrap/expect allowed

## Preferences

- Real arm64 device (Samsung), not emulator
- Use `gradlew installDebug` (not separate build + adb install)
- Don't overwrite PATH in .env, just append
- Debug builds for development iteration
