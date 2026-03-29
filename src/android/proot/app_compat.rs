//! App compatibility shims for proot.
//!
//! Firefox, Chromium/Electron, glycin (bwrap), flatpak-spawn, bsdtar, and
//! ttyname all need workarounds to function inside a proot environment.
//! Each function is idempotent and skips if the fix is already in place.

use super::process::ArchProcess;
use super::setup::setup_log;
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
    let glxtest_script = r#"#!/bin/sh
# EGL-based GPU probe replacement for Firefox's glxtest (which fails in proot).
# Firefox opens fd 3 as a pipe before launching this. Write GPU info there.
info=$(eglinfo -B 2>/dev/null)
vendor=$(echo "$info" | grep "OpenGL core profile vendor:" | head -1 | sed 's/.*: //')
renderer=$(echo "$info" | grep "OpenGL core profile renderer:" | head -1 | sed 's/.*: //')
version=$(echo "$info" | grep "OpenGL core profile version:" | head -1 | sed 's/.*: //')
if [ -n "$renderer" ]; then
    printf "VENDOR\n%s\nRENDERER\n%s\nVERSION\n%s\nTFP\nEGL\n" "$vendor" "$renderer" "$version" >&3
fi
"#;
    fs::write(&glxtest, glxtest_script)
        .unwrap_or_else(|e| tracing::error!("[setup] Failed to write glxtest: {}", e));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&glxtest, fs::Permissions::from_mode(0o755));
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

    // Already our shim
    if bwrap.exists() {
        if let Ok(contents) = fs::read(&bwrap) {
            if contents.starts_with(b"#!/bin/sh") {
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

    let shim = r#"#!/usr/bin/env python3
"""bwrap shim: uses nested proot for bind mounts instead of namespaces.
Handles --args FD (NUL-separated args from file descriptor) used by flatpak.
Requires _PROOT_BIN, _PROOT_LOADER, _PROOT_TMP_DIR env vars."""
import sys, os

def read_fd(fd):
    data = b''
    while True:
        chunk = os.read(fd, 4096)
        if not chunk: break
        data += chunk
    os.close(fd)
    return data

args = list(sys.argv[1:])
# Expand --args FD
i = 0
while i < len(args):
    if args[i] == '--args' and i + 1 < len(args):
        fd = int(args[i + 1])
        rest = args[i + 2:]
        decoded = read_fd(fd).decode('utf-8', errors='replace')
        extra = decoded.split('\0')
        if extra and extra[-1] == '':
            extra.pop()
        args = extra + rest
        i = 0
        continue
    i += 1

clear_env = False
chdir_path = None
cmd = []
binds = []
env_set = {}
env_unset = []
i = 0

ONE_ARG = {'--unshare-all', '--unshare-user', '--unshare-user-try', '--unshare-ipc',
    '--unshare-pid', '--unshare-net', '--unshare-uts', '--unshare-cgroup',
    '--unshare-cgroup-try', '--share-net', '--die-with-parent', '--new-session',
    '--as-pid-1', '--disable-userns', '--assert-userns-disabled'}
TWO_ARG = {'--lock-file', '--sync-fd', '--info-fd', '--json-status-fd', '--block-fd',
    '--userns-block-fd', '--size', '--perms', '--uid', '--gid', '--hostname',
    '--exec-label', '--file-label', '--cap-add', '--cap-drop',
    '--seccomp', '--userns', '--userns2', '--pidns'}

while i < len(args):
    a = args[i]
    if a == '--':
        cmd = args[i + 1:]
        break
    elif a == '--setenv' and i + 2 < len(args):
        env_set[args[i + 1]] = args[i + 2]; i += 3
    elif a == '--unsetenv' and i + 1 < len(args):
        env_unset.append(args[i + 1]); i += 2
    elif a == '--chdir' and i + 1 < len(args):
        chdir_path = args[i + 1]; i += 2
    elif a == '--clearenv':
        clear_env = True; i += 1
    elif a == '--file' and i + 2 < len(args):
        dest = args[i + 2]
        try:
            os.makedirs(os.path.dirname(dest) or '.', exist_ok=True)
            with open(dest, 'wb') as f: f.write(read_fd(int(args[i + 1])))
        except OSError: pass
        i += 3
    elif a in ('--bind-data', '--ro-bind-data') and i + 2 < len(args):
        dest = args[i + 2]
        try:
            os.makedirs(os.path.dirname(dest) or '.', exist_ok=True)
            with open(dest, 'wb') as f: f.write(read_fd(int(args[i + 1])))
        except OSError: pass
        i += 3
    elif a in ('--ro-bind', '--bind', '--ro-bind-try', '--bind-try',
               '--dev-bind', '--dev-bind-try') and i + 2 < len(args):
        src, dest = args[i + 1], args[i + 2]
        if src != dest:
            binds.append((src, dest))
        i += 3
    elif a == '--remount-ro-bind' and i + 2 < len(args):
        i += 3
    elif a == '--remount-ro' and i + 1 < len(args):
        i += 2
    elif a == '--dir' and i + 1 < len(args):
        try: os.makedirs(args[i + 1], exist_ok=True)
        except OSError: pass
        i += 2
    elif a == '--tmpfs' and i + 1 < len(args):
        try: os.makedirs(args[i + 1], exist_ok=True)
        except OSError: pass
        i += 2
    elif a == '--symlink' and i + 2 < len(args):
        try:
            os.makedirs(os.path.dirname(args[i + 2]) or '.', exist_ok=True)
            if os.path.lexists(args[i + 2]): os.unlink(args[i + 2])
            os.symlink(args[i + 1], args[i + 2])
        except OSError: pass
        i += 3
    elif a == '--chmod' and i + 2 < len(args):
        try: os.chmod(args[i + 2], int(args[i + 1], 8))
        except OSError: pass
        i += 3
    elif a in ('--dev', '--proc', '--mqueue') and i + 1 < len(args):
        i += 2
    elif a in ONE_ARG: i += 1
    elif a in TWO_ARG and i + 1 < len(args): i += 2
    elif not a.startswith('--'):
        cmd = args[i:]
        break
    else: i += 1

if not cmd:
    sys.exit(0)

# Build environment
if clear_env:
    env = {}
else:
    env = dict(os.environ)
for k, v in env_set.items():
    env[k] = v
for k in env_unset:
    env.pop(k, None)

internal_keys = ('_PROOT_BIN', '_PROOT_LOADER', '_PROOT_TMP_DIR')

# Find proot binary: prefer env var, fall back to scanning /proc
proot_bin = os.environ.get('_PROOT_BIN', '')
proot_loader = os.environ.get('_PROOT_LOADER', '')
proot_tmp = os.environ.get('_PROOT_TMP_DIR', '/tmp')
if not proot_bin:
    try:
        for entry in os.listdir('/proc'):
            if entry.isdigit():
                try:
                    exe = os.readlink(f'/proc/{entry}/exe')
                    if exe.endswith('/libproot.so'):
                        proot_bin = exe
                        proot_loader = exe.replace('libproot.so', 'libproot_loader.so')
                        break
                except OSError:
                    continue
    except OSError:
        pass

# Use nested proot for bind mounts when available
if binds and proot_bin and os.path.isfile(proot_bin):
    proot_args = [proot_bin, '-r', '/', '-L', '--link2symlink']
    for src, dest in binds:
        proot_args.append(f'--bind={src}:{dest}')
    if chdir_path:
        proot_args.extend(['-w', chdir_path])
    proot_args.extend(cmd)
    env['PROOT_LOADER'] = proot_loader
    env['PROOT_TMP_DIR'] = proot_tmp
    for k in internal_keys: env.pop(k, None)
    os.execvpe(proot_args[0], proot_args, env)
else:
    if chdir_path:
        try: os.chdir(chdir_path)
        except OSError: pass
    for k in internal_keys: env.pop(k, None)
    os.execvpe(cmd[0], cmd, env)
"#;

    setup_log("[setup] Installing bwrap shim (sandboxing incompatible with proot)");
    if let Err(e) = fs::write(&bwrap, shim) {
        tracing::error!("[setup] Failed to write bwrap shim: {}", e);
        return;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&bwrap, fs::Permissions::from_mode(0o755));
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

    let shim = r#"#!/bin/sh
# flatpak-spawn shim: runs the command unsandboxed (proot can't do namespaces).
# Strips all flatpak-spawn options and execs the trailing command.
dir=""
while [ $# -gt 0 ]; do
    case "$1" in
        --sandbox|--watch-bus|--latest-version|--no-network|--clear-env|--host|--verbose)
            shift ;;
        --directory=*)
            dir="${1#--directory=}"; shift ;;
        --forward-fd=*|--env=*)
            shift ;;
        -*)
            shift ;;
        *)
            break ;;
    esac
done
if [ -n "$dir" ]; then
    cd "$dir" 2>/dev/null || true
fi
exec "$@"
"#;

    setup_log("[setup] Installing flatpak-spawn shim (sandboxing incompatible with proot)");
    if let Err(e) = fs::write(&flatpak_spawn, shim) {
        tracing::error!("[setup] Failed to write flatpak-spawn shim: {}", e);
        return;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&flatpak_spawn, fs::Permissions::from_mode(0o755));
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
    let shim = "#!/bin/sh\n/usr/bin/bsdtar \"$@\"\nexit 0\n";
    if let Err(e) = fs::write(&wrapper, shim) {
        tracing::error!("[setup] Failed to write bsdtar wrapper: {}", e);
        return;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755));
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
    let source_code = r#"#define _GNU_SOURCE
#include <unistd.h>
#include <stdio.h>
#include <string.h>
#include <errno.h>
#include <sys/stat.h>

/* Override ttyname_r to use /proc/self/fd instead of scanning /dev/pts/.
 * On Android, SELinux blocks readdir on /dev/pts for untrusted_app,
 * causing ttyname_r to fail with EACCES. */
int ttyname_r(int fd, char *buf, size_t buflen) {
    struct stat st;
    if (fstat(fd, &st) != 0 || !S_ISCHR(st.st_mode)) {
        errno = ENOTTY;
        return ENOTTY;
    }
    char proc_path[64];
    snprintf(proc_path, sizeof(proc_path), "/proc/self/fd/%d", fd);
    ssize_t len = readlink(proc_path, buf, buflen - 1);
    if (len == -1) return errno;
    buf[len] = '\0';
    return 0;
}

char *ttyname(int fd) {
    static char buf[256];
    if (ttyname_r(fd, buf, sizeof(buf)) != 0) return NULL;
    return buf;
}
"#;

    if let Err(e) = fs::write(&c_source, source_code) {
        tracing::error!("[setup] Failed to write fix_ttyname.c: {}", e);
        return;
    }

    let output = ArchProcess {
        command: "gcc -shared -fPIC -o /usr/lib/fix_ttyname.so /tmp/fix_ttyname.c && echo OK"
            .into(),
        user: None,
        log: None,
        kill_on_exit: true,
    }
    .run();

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
