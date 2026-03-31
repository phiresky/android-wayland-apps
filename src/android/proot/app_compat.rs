//! App compatibility shims for proot.
//!
//! Firefox, Chromium/Electron, glycin (bwrap), flatpak-spawn, bsdtar, and
//! ttyname all need workarounds to function inside a proot environment.
//! Each function is idempotent and skips if the fix is already in place.

use super::process::ArchProcess;
use super::setup::{setup_log, write_executable};
use crate::core::config;
use std::fs;
use std::path::Path;

/// Configure Firefox to work inside proot.
///
/// Firefox's content process sandbox uses Linux namespaces and seccomp-bpf,
/// which don't work inside proot. Without this config, every tab crashes.
/// Uses Firefox's autoconfig mechanism (same approach as localdesktop).
pub fn setup_firefox_config() {
    let fs_root = Path::new(config::ARCH_FS_ROOT);
    let firefox_root = fs_root.join("usr/lib/firefox");
    let cfg_file = firefox_root.join("wayland_android.cfg");

    if !firefox_root.exists() {
        // Firefox not installed yet, skip
        return;
    }

    setup_log("[setup] Configuring Firefox for proot compatibility...");

    let pref_dir = firefox_root.join("defaults/pref");
    let _ = fs::create_dir_all(&pref_dir);

    // autoconfig.js tells Firefox to load our .cfg file
    let autoconfig_js = "pref(\"general.config.filename\", \"wayland_android.cfg\");\n\
                         pref(\"general.config.obscure_value\", 0);\n";
    fs::write(pref_dir.join("autoconfig.js"), autoconfig_js)
        .unwrap_or_else(|e| tracing::error!("[setup] Failed to write autoconfig.js: {}", e));

    // The .cfg file must start with a comment line (Firefox requirement)
    let cfg = "\
// Auto-configured by wayland_android for proot compatibility
defaultPref(\"security.sandbox.content.level\", 0);
defaultPref(\"media.cubeb.sandbox\", false);
defaultPref(\"security.sandbox.warn_unprivileged_namespaces\", false);
defaultPref(\"gfx.webrender.all\", true);
defaultPref(\"gfx.webrender.software\", true);
defaultPref(\"widget.gtk.overlay-scrollbars.enabled\", false);
// defaultPref(\"widget.non-native-theme.gtk.scrollbar.thumb-size\", \"1\");
defaultPref(\"widget.non-native-theme.scrollbar.size.override\", 16);
";
    fs::write(&cfg_file, cfg)
        .unwrap_or_else(|e| tracing::error!("[setup] Failed to write Firefox config: {}", e));

    // Restore Firefox binary if we previously replaced it with a wrapper.
    let real_firefox = firefox_root.join("firefox");
    let real_firefox_bin = firefox_root.join("firefox.real");
    if real_firefox_bin.exists() {
        // Check if current "firefox" is a shell script wrapper
        if let Ok(content) = fs::read(&real_firefox) {
            if content.starts_with(b"#!/bin/sh") {
                let _ = fs::remove_file(&real_firefox);
                let _ = fs::rename(&real_firefox_bin, &real_firefox);
                setup_log("[setup] Restored Firefox binary (removed wrapper)");
            }
        }
    }

    // Replace glxtest with an EGL-based probe. Firefox's glxtest binary
    // crashes in proot (seccomp/fork issues), causing GPU detection to fail
    // and WebGL to be disabled. This script probes EGL via eglinfo and writes
    // the expected format to fd 3.
    let glxtest = firefox_root.join("glxtest");
    let glxtest_orig = firefox_root.join("glxtest.orig");
    if glxtest.exists() && !glxtest_orig.exists() {
        let _ = fs::rename(&glxtest, &glxtest_orig);
    }
    if let Err(e) = write_executable(&glxtest, include_str!("scripts/glxtest_egl.sh")) {
        tracing::error!("[setup] Failed to write glxtest: {}", e);
    }
}

/// Configure Chromium-based apps to run without sandbox.
/// Chromium's sandbox uses seccomp/namespaces that don't work in proot.
pub fn setup_electron_config() {
    let config_dir = Path::new(config::ARCH_FS_ROOT)
        .join("home")
        .join(config::USERNAME)
        .join(".config");
    let _ = fs::create_dir_all(&config_dir);

    let flags = "--no-sandbox\n--ozone-platform=wayland\n";
    for name in ["chromium-flags.conf", "code-flags.conf", "electron-flags.conf"] {
        fs::write(config_dir.join(name), flags)
            .unwrap_or_else(|e| tracing::error!("[setup] Failed to write {}: {}", name, e));
    }
}

/// Replace bwrap (bubblewrap) with a shim that runs commands unsandboxed.
///
/// glycin (gdk-pixbuf image loader) invokes bwrap to sandbox sub-processes.
/// bwrap uses Linux namespaces (--unshare-all) which don't work inside proot.
/// glycin doesn't fall back when bwrap is missing — it just crashes.
/// The shim parses bwrap's options, applies --setenv, then execs the command.
pub fn disable_bwrap() {
    let bwrap = Path::new(config::ARCH_FS_ROOT).join("usr/bin/bwrap");
    let bwrap_real = Path::new(config::ARCH_FS_ROOT).join("usr/bin/bwrap.real");

    // Already our shim (Python or shell script, not an ELF binary)
    if bwrap.exists() {
        if let Ok(contents) = fs::read(&bwrap) {
            if contents.starts_with(b"#!") {
                return;
            }
        }
        // Real binary — move it aside
        if let Err(e) = fs::rename(&bwrap, &bwrap_real) {
            tracing::error!("[setup] Failed to rename bwrap: {}", e);
            return;
        }
    }

    if !bwrap_real.exists() {
        return;
    }

    setup_log("[setup] Installing bwrap shim (sandboxing incompatible with proot)");
    if let Err(e) = write_executable(&bwrap, include_str!("scripts/bwrap_shim.py")) {
        tracing::error!("[setup] Failed to write bwrap shim: {}", e);
    }
}

/// Replace flatpak-spawn with a shim that runs commands unsandboxed.
///
/// glycin (gdk-pixbuf image loader) may invoke flatpak-spawn instead of bwrap
/// to sandbox sub-processes. flatpak-spawn doesn't exist in the rootfs and
/// glycin crashes if it's missing. The shim strips all options and execs the command.
pub fn disable_flatpak_spawn() {
    let flatpak_spawn = Path::new(config::ARCH_FS_ROOT).join("usr/bin/flatpak-spawn");

    // Already our shim
    if flatpak_spawn.exists() {
        if let Ok(contents) = fs::read(&flatpak_spawn) {
            if contents.starts_with(b"#!/bin/sh") {
                return;
            }
        }
    }

    setup_log("[setup] Installing flatpak-spawn shim (sandboxing incompatible with proot)");
    if let Err(e) = write_executable(&flatpak_spawn, include_str!("scripts/flatpak_spawn_shim.sh")) {
        tracing::error!("[setup] Failed to write flatpak-spawn shim: {}", e);
    }
}

/// Wrap bsdtar so permission errors don't abort makepkg source extraction.
///
/// proot fakes root with `--root-id` but the Android filesystem still rejects
/// `chmod()` on symlink targets that haven't been extracted yet (ENOENT).
/// pacman tolerates these warnings, but makepkg checks bsdtar's exit code
/// and aborts on any error. The wrapper runs the real bsdtar and exits 0.
pub(super) fn fix_bsdtar() {
    let fs_root = Path::new(config::ARCH_FS_ROOT);
    let wrapper = fs_root.join("usr/local/bin/bsdtar");
    let real = fs_root.join("usr/bin/bsdtar");

    if wrapper.exists() {
        return;
    }
    if !real.exists() {
        return;
    }

    setup_log("[setup] Installing bsdtar wrapper (permission errors non-fatal)");

    let _ = fs::create_dir_all(fs_root.join("usr/local/bin"));
    if let Err(e) = write_executable(&wrapper, include_str!("scripts/bsdtar_wrapper.sh")) {
        tracing::error!("[setup] Failed to write bsdtar wrapper: {}", e);
    }
}

/// Build a ttyname_r shim library for LD_PRELOAD inside proot.
///
/// Android's SELinux policy blocks `readdir` on `/dev/pts` for untrusted_app
/// domains. The libc `ttyname_r()` function scans that directory to resolve PTY
/// slave names, so it fails with EACCES. Programs like kitty call `ttyname_r`
/// before spawning child processes and abort on failure.
///
/// The shim overrides `ttyname_r` (and `ttyname`) to read `/proc/self/fd/<fd>`
/// via `readlink` instead, which is not blocked by SELinux.
pub(super) fn fix_ttyname() {
    let fs_root = Path::new(config::ARCH_FS_ROOT);
    let so_path = fs_root.join("usr/lib/fix_ttyname.so");
    if so_path.exists() {
        return;
    }

    setup_log("[setup] Building ttyname fix for Android SELinux...");

    let c_source = fs_root.join("tmp/fix_ttyname.c");
    if let Err(e) = fs::write(&c_source, include_str!("scripts/fix_ttyname.c")) {
        tracing::error!("[setup] Failed to write fix_ttyname.c: {}", e);
        return;
    }

    let output = ArchProcess::run_simple(
        "gcc -shared -fPIC -o /usr/lib/fix_ttyname.so /tmp/fix_ttyname.c && echo OK",
    );

    if output.status.success()
        && String::from_utf8_lossy(&output.stdout).contains("OK")
    {
        setup_log("[setup] ttyname fix built successfully");
    } else {
        tracing::error!(
            "[setup] Failed to build fix_ttyname.so: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let _ = fs::remove_file(&c_source);
}
