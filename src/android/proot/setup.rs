use super::process::ArchProcess;
use crate::{android::utils::application_context::get_application_context, core::config};
use pathdiff::diff_paths;
use std::{
    fs::{self, File},
    io::{Read, Write},
    os::unix::fs::symlink,
    path::Path,
    sync::{Arc, Mutex},
};
use tar::Archive;
use xz2::read::XzDecoder;

const MAX_INSTALL_ATTEMPTS: usize = 10;

// Optional UI logger callback — set by main.rs to forward logs to SetupOverlay.
static UI_LOGGER: Mutex<Option<Box<dyn Fn(&str) + Send>>> = Mutex::new(None);

pub fn set_ui_logger(f: impl Fn(&str) + Send + 'static) {
    if let Ok(mut guard) = UI_LOGGER.lock() {
        *guard = Some(Box::new(f));
    }
}

pub fn clear_ui_logger() {
    if let Ok(mut guard) = UI_LOGGER.lock() {
        *guard = None;
    }
}

/// Log to both logcat and the optional UI logger.
fn setup_log(msg: &str) {
    tracing::info!("{}", msg);
    if let Ok(guard) = UI_LOGGER.lock()
        && let Some(f) = guard.as_ref() {
            f(msg);
        }
}

/// Check if setup is fully complete (rootfs extracted AND dependencies installed).
pub fn is_setup_complete() -> bool {
    let fs_root = Path::new(config::ARCH_FS_ROOT);
    if !fs_root.join("usr/bin").exists() {
        return false;
    }
    if !fs_root.join(format!("etc/sudoers.d/{}", config::USERNAME)).exists() {
        return false;
    }
    if !fs_root.join("usr/local/bin/bsdtar").exists() {
        return false;
    }
    ArchProcess {
        command: config::check_cmd(),
        user: None,
        log: None,
        kill_on_exit: true,
    }
    .run()
    .status
    .success()
}

/// Run all proot setup stages sequentially. Each stage is idempotent
/// and skips if already done. Progress is logged to logcat and optionally to the UI.
pub fn run_setup() {
    if !ArchProcess::is_supported() {
        setup_log("[setup] PRoot is not supported on this device");
        return;
    }
    setup_log("[setup] Your device is supported!");

    setup_log("=== Proot setup starting ===");
    setup_arch_fs();
    setup_sysdata();
    setup_machine_id();
    setup_dns();
    setup_alpm_user();
    install_dependencies();
    setup_user();
    disable_bwrap();
    disable_flatpak_spawn();
    fix_bsdtar();
    setup_firefox_config();
    setup_electron_config();
    fix_xkb_symlink();
    fix_ttyname();
    setup_storage_mountpoints();
    if config::pipewire_enabled() {
        setup_pipewire_config();
    }
    setup_flatpak_dbus();
    setup_flatpak_system_repo();
    setup_portal();
    setup_hybris_vulkan();
    setup_log("=== Proot setup complete ===");
}

/// Download and extract the Arch Linux rootfs if not already present.
fn setup_arch_fs() {
    let fs_root = Path::new(config::ARCH_FS_ROOT);
    if fs_root.join("usr/bin").exists() {
        setup_log("[setup] Arch rootfs already present, skipping download");
        return;
    }

    let context = get_application_context();
    let temp_file = context.cache_dir.join("archlinux-fs.tar.xz");
    let extracted_dir = context.cache_dir.join("archlinux-aarch64");

    loop {
        // Download if the archive doesn't exist
        if !temp_file.exists() {
            setup_log(&format!(
                "[setup] Downloading Arch Linux FS from {}...",
                config::ARCH_FS_ARCHIVE
            ));

            let response = match ureq::get(config::ARCH_FS_ARCHIVE).call() {
                Ok(r) => r,
                Err(e) => {
                    setup_log(&format!("[setup] Download failed: {}. Retrying...", e));
                    continue;
                }
            };

            let total_size: u64 = response
                .header("content-length")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);

            let mut reader = response.into_reader();
            let mut file = match File::create(&temp_file) {
                Ok(f) => f,
                Err(e) => {
                    setup_log(&format!("[setup] Failed to create temp file: {}", e));
                    return;
                }
            };
            let mut downloaded = 0u64;
            let mut buffer = [0u8; 8192];
            let mut last_percent = 0u8;
            let mut download_ok = true;

            loop {
                let n = match reader.read(&mut buffer) {
                    Ok(n) => n,
                    Err(e) => {
                        setup_log(&format!("[setup] Read error during download: {}", e));
                        download_ok = false;
                        break;
                    }
                };
                if n == 0 {
                    break;
                }
                if let Err(e) = file.write_all(&buffer[..n]) {
                    setup_log(&format!("[setup] Write error during download: {}", e));
                    download_ok = false;
                    break;
                }
                downloaded += n as u64;
                if total_size > 0 {
                    let percent = (downloaded * 100 / total_size).min(100) as u8;
                    if percent != last_percent {
                        setup_log(&format!(
                            "[setup] Downloading... {}% ({:.1} MB / {:.1} MB)",
                            percent,
                            downloaded as f64 / 1_048_576.0,
                            total_size as f64 / 1_048_576.0
                        ));
                        last_percent = percent;
                    }
                }
            }

            if !download_ok {
                let _ = fs::remove_file(&temp_file);
                continue;
            }
        }

        setup_log("[setup] Extracting Arch Linux FS...");
        let _ = fs::remove_dir_all(&extracted_dir);

        let tar_file = match File::open(&temp_file) {
            Ok(f) => f,
            Err(e) => {
                setup_log(&format!("[setup] Failed to open archive: {}", e));
                return;
            }
        };
        let tar = XzDecoder::new(tar_file);
        let mut archive = Archive::new(tar);

        let mut extract_ok = true;
        let mut entry_count = 0u32;
        match archive.entries() {
            Ok(entries) => {
                for entry in entries {
                    match entry {
                        Ok(mut e) => {
                            if let Err(err) = e.unpack_in(&context.cache_dir) {
                                setup_log(&format!("[setup] Extraction error: {}", err));
                                extract_ok = false;
                                break;
                            }
                            entry_count += 1;
                            if entry_count.is_multiple_of(2000) {
                                setup_log(&format!(
                                    "[setup] Extracted {} files...",
                                    entry_count
                                ));
                            }
                        }
                        Err(e) => {
                            setup_log(&format!("[setup] Tar entry error: {}", e));
                            extract_ok = false;
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                setup_log(&format!("[setup] Failed to read archive: {}", e));
                extract_ok = false;
            }
        }

        if !extract_ok {
            let _ = fs::remove_dir_all(&extracted_dir);
            let _ = fs::remove_file(&temp_file);
            setup_log("[setup] Extraction failed. Retrying download...");
            continue;
        }
        setup_log(&format!("[setup] Extracted {} files total", entry_count));

        break;
    }

    let _ = fs::remove_dir_all(fs_root);
    if let Err(e) = fs::rename(&extracted_dir, fs_root) {
        setup_log(&format!("[setup] Failed to move rootfs to final location: {}", e));
        return;
    }
    let _ = fs::remove_file(&temp_file);
    setup_log("[setup] Arch rootfs extracted successfully");
}

/// Create fake /proc and /sys files needed by proot.
fn setup_sysdata() {
    let fs_root = Path::new(config::ARCH_FS_ROOT);
    if fs_root.join("proc/.version").exists() {
        return;
    }

    setup_log("[setup] Creating fake Linux system data...");
    let _ = fs::create_dir_all(fs_root.join("proc"));
    let _ = fs::create_dir_all(fs_root.join("sys/.empty"));

    let proc_files = [
        ("proc/.loadavg", "0.12 0.07 0.02 2/165 765\n"),
        ("proc/.stat", "cpu  1957 0 2877 93280 262 342 254 87 0 0\ncpu0 31 0 226 12027 82 10 4 9 0 0\n"),
        ("proc/.uptime", "124.08 932.80\n"),
        ("proc/.version", "Linux version 6.2.1 (proot@termux) (gcc (GCC) 12.2.1 20230201, GNU ld (GNU Binutils) 2.40) #1 SMP PREEMPT_DYNAMIC Wed, 01 Mar 2023 00:00:00 +0000\n"),
        ("proc/.vmstat", "nr_free_pages 1743136\nnr_zone_inactive_anon 179281\nnr_zone_active_anon 7183\n"),
        ("proc/.sysctl_entry_cap_last_cap", "40\n"),
        ("proc/.sysctl_inotify_max_user_watches", "4096\n"),
    ];

    for (path, content) in proc_files {
        fs::write(fs_root.join(path), content)
            .unwrap_or_else(|e| tracing::error!("[setup] Failed to write {}: {}", path, e));
    }
}

/// Generate /etc/machine-id if missing.
///
/// Normally created by systemd on first boot, but proot never runs an init system.
/// Required by dbus (Firefox, GTK apps) — without it they log warnings or fail.
fn setup_machine_id() {
    let machine_id = Path::new(config::ARCH_FS_ROOT).join("etc/machine-id");
    if machine_id.exists() {
        return;
    }

    setup_log("[setup] Generating /etc/machine-id...");

    ArchProcess {
        command: "systemd-machine-id-setup".into(),
        user: None,
        log: None,
        kill_on_exit: true,
    }
    .run();
}

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
    let cfg = "// Auto-configured by wayland_android for proot compatibility\n\
               defaultPref(\"security.sandbox.content.level\", 0);\n\
               defaultPref(\"media.cubeb.sandbox\", false);\n\
               defaultPref(\"security.sandbox.warn_unprivileged_namespaces\", false);\n";
    fs::write(&cfg_file, cfg)
        .unwrap_or_else(|e| tracing::error!("[setup] Failed to write Firefox config: {}", e));

}

/// Configure Electron apps (VSCode etc.) to run without sandbox.
/// Electron's Chromium sandbox uses seccomp/namespaces that don't work in proot.
fn setup_electron_config() {
    let config_dir = Path::new(config::ARCH_FS_ROOT)
        .join("home")
        .join(config::USERNAME)
        .join(".config");
    let _ = fs::create_dir_all(&config_dir);

    let flags = "--no-sandbox\n";
    for name in ["code-flags.conf", "electron-flags.conf"] {
        fs::write(config_dir.join(name), flags)
            .unwrap_or_else(|e| tracing::error!("[setup] Failed to write {}: {}", name, e));
    }
}

/// Ensure resolv.conf exists with a working nameserver.
/// glibc inside proot needs this for DNS resolution.
fn setup_dns() {
    let resolv_conf = Path::new(config::ARCH_FS_ROOT).join("etc/resolv.conf");
    // Check if a nameserver is already configured (file may exist with only comments).
    if let Ok(contents) = fs::read_to_string(&resolv_conf) {
        if contents.lines().any(|l| l.trim_start().starts_with("nameserver")) {
            return;
        }
    }
    setup_log("[setup] Writing resolv.conf...");
    if let Some(parent) = resolv_conf.parent() {
        let _ = fs::create_dir_all(parent);
    }
    fs::write(&resolv_conf, "nameserver 8.8.8.8\n")
        .unwrap_or_else(|e| tracing::error!("[setup] Failed to write resolv.conf: {}", e));
}

/// Create the `alpm` user/group required by pacman >= 7.0 for downloads.
/// Without this user, pacman fails with "problem setting DownloadUser 'alpm'".
fn setup_alpm_user() {
    let fs_root = Path::new(config::ARCH_FS_ROOT);
    let passwd = fs_root.join("etc/passwd");
    let group = fs_root.join("etc/group");

    // Check if alpm already exists
    if let Ok(content) = fs::read_to_string(&passwd) {
        if content.contains("alpm:") {
            return;
        }
    }

    setup_log("[setup] Creating alpm user for pacman downloads...");

    // Append alpm group (GID 946 is Arch's standard)
    if let Ok(mut content) = fs::read_to_string(&group) {
        if !content.contains("alpm:") {
            if !content.ends_with('\n') {
                content.push('\n');
            }
            content.push_str("alpm:x:946:\n");
            let _ = fs::write(&group, content);
        }
    }

    // Append alpm user (UID 946)
    if let Ok(mut content) = fs::read_to_string(&passwd) {
        if !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str("alpm:x:946:946:Arch Linux Package Manager:/:/usr/bin/nologin\n");
        let _ = fs::write(&passwd, content);
    }
}

/// Install dependencies via pacman if the check command fails.
fn install_dependencies() {
    let is_installed = || {
        ArchProcess {
            command: config::check_cmd(),
            user: None,
            log: None,
            kill_on_exit: true,
        }
        .run()
        .status
        .success()
    };

    if is_installed() {
        setup_log("[setup] Dependencies already installed");
        return;
    }

    for attempt in 1..=MAX_INSTALL_ATTEMPTS {
        setup_log(&format!(
            "[setup] Installing dependencies (attempt {}/{})...",
            attempt, MAX_INSTALL_ATTEMPTS
        ));

        // Remove stale pacman lock
        ArchProcess {
            command: "rm -f /var/lib/pacman/db.lck".into(),
            user: None,
            log: None,
            kill_on_exit: true,
        }
        .run();

        // Run install command with output logged to logcat and UI
        ArchProcess {
            command: config::install_cmd(),
            user: None,
            log: Some(Arc::new(|line| setup_log(&format!("[pacman] {}", line)))),
            kill_on_exit: true,
        }
        .run();

        if is_installed() {
            setup_log("[setup] Dependencies installed successfully");
            return;
        }

        if attempt == MAX_INSTALL_ATTEMPTS {
            setup_log(&format!(
                "[setup] Failed to install dependencies after {} attempts. \
                 The app will start but the launch command may fail.",
                MAX_INSTALL_ATTEMPTS
            ));
        }
    }
}

/// Grant the default user passwordless sudo.
fn setup_user() {
    let user = config::USERNAME;
    let fs_root = Path::new(config::ARCH_FS_ROOT);
    let sudoers_file = fs_root.join(format!("etc/sudoers.d/{}", user));

    if sudoers_file.exists() {
        return;
    }

    setup_log(&format!("[setup] Configuring sudo for '{}'...", user));

    let sudoers_dir = fs_root.join("etc/sudoers.d");
    let _ = fs::create_dir_all(&sudoers_dir);
    fs::write(
        &sudoers_file,
        format!("{} ALL=(ALL) NOPASSWD: ALL\n", user),
    )
    .unwrap_or_else(|e| tracing::error!("[setup] Failed to write sudoers for {}: {}", user, e));
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
fn fix_bsdtar() {
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
fn fix_ttyname() {
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

/// Create bind mount target directories for Android storage inside the rootfs.
///
/// /storage/emulated/0 and /sdcard are not present in the Arch tarball.
/// proot requires the destination directory to exist before binding.
fn setup_storage_mountpoints() {
    let fs_root = Path::new(config::ARCH_FS_ROOT);
    let dirs = [
        "sdcard",
        "storage/emulated/0",
    ];
    for dir in dirs {
        let path = fs_root.join(dir);
        if !path.exists() {
            if let Err(e) = fs::create_dir_all(&path) {
                tracing::error!("[setup] Failed to create /{}: {}", dir, e);
            }
        }
    }
}


/// Configure PipeWire for unrestricted access inside proot.
/// The default access module tries flatpak/portal checks which fail in proot,
/// causing clients (pw-cli, apps) to hang. Uncomment and set socket-based
/// access to "unrestricted" in the main config.
fn setup_pipewire_config() {
    let conf_path = Path::new(config::ARCH_FS_ROOT)
        .join("usr/share/pipewire/pipewire.conf");
    if !conf_path.exists() {
        return;
    }
    let Ok(conf) = fs::read_to_string(&conf_path) else { return };
    let patched = conf.replace(
        "#access.socket = { pipewire-0 = \"default\", pipewire-0-manager = \"unrestricted\" }",
        "access.socket = { pipewire-0 = \"unrestricted\", pipewire-0-manager = \"unrestricted\" }",
    );
    if patched != conf {
        if let Err(e) = fs::write(&conf_path, &patched) {
            tracing::error!("[setup] Failed to patch pipewire.conf: {e}");
        }
    }
}

/// Install a custom D-Bus system bus config and a helper script for flatpak.
///
/// The default dbus system config tries to switch to user `dbus` and drop
/// capabilities, which fails inside proot. We use a `custom` bus type that
/// skips privilege dropping but listens on the standard system socket path.
///
/// The `start-dbus` script starts system + session buses and exports the
/// required environment variables. Sourced by adb_runas.sh and compositor
/// app launches.
pub fn setup_flatpak_dbus() {
    let fs_root = Path::new(config::ARCH_FS_ROOT);
    let dbus_conf = fs_root.join("etc/dbus-1/proot-system.conf");
    let start_dbus = fs_root.join("usr/local/bin/start-dbus");

    // Re-generate start-dbus if it doesn't use our custom session config
    let needs_update = start_dbus.exists()
        && fs::read_to_string(&start_dbus)
            .map(|s| !s.contains("proot-session.conf"))
            .unwrap_or(false);

    if dbus_conf.exists() && start_dbus.exists() && !needs_update {
        return;
    }

    setup_log("[setup] Configuring D-Bus for flatpak support...");

    // Custom system bus config that doesn't drop capabilities
    let _ = fs::create_dir_all(fs_root.join("etc/dbus-1"));
    let conf = r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>custom</type>
  <listen>unix:path=/run/dbus/system_bus_socket</listen>
  <auth>EXTERNAL</auth>
  <allow_anonymous/>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
    <allow user="*"/>
  </policy>
</busconfig>
"#;
    fs::write(&dbus_conf, conf)
        .unwrap_or_else(|e| tracing::error!("[setup] Failed to write dbus config: {}", e));

    // Custom session bus config — anonymous auth so D-Bus works across proot instances.
    // Default session config uses EXTERNAL auth which relies on SCM_CREDENTIALS,
    // but proot's ptrace interception corrupts credential ancillary data.
    let session_conf = fs_root.join("etc/dbus-1/proot-session.conf");
    let session_conf_content = r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>custom</type>
  <listen>unix:path=/tmp/dbus-session-bus-socket</listen>
  <auth>ANONYMOUS</auth>
  <allow_anonymous/>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
    <allow user="*"/>
  </policy>

  <servicedir>/usr/share/dbus-1/services</servicedir>
  <servicedir>/usr/local/share/dbus-1/services</servicedir>
</busconfig>
"#;
    fs::write(&session_conf, session_conf_content)
        .unwrap_or_else(|e| tracing::error!("[setup] Failed to write session dbus config: {}", e));

    // Helper script to start both buses (idempotent)
    let _ = fs::create_dir_all(fs_root.join("usr/local/bin"));
    let script = r#"#!/bin/sh
# Start D-Bus system and session buses for proot (idempotent).
# Source this: . start-dbus

# XDG_RUNTIME_DIR must be user-private (dbus requires mode 700)
_uid=$(id -u)
export XDG_RUNTIME_DIR="/tmp/runtime-${_uid}"
mkdir -p "$XDG_RUNTIME_DIR" 2>/dev/null
chmod 700 "$XDG_RUNTIME_DIR" 2>/dev/null

# System bus — verify it's actually connectable (socket may be stale)
if ! dbus-send --system --dest=org.freedesktop.DBus /org/freedesktop/DBus org.freedesktop.DBus.GetId >/dev/null 2>&1; then
    rm -f /run/dbus/system_bus_socket /run/dbus/pid
    mkdir -p /run/dbus 2>/dev/null
    dbus-daemon --config-file=/etc/dbus-1/proot-system.conf --nofork --nopidfile &
    # Brief wait for socket to appear
    _i=0; while [ "$_i" -lt 10 ] && [ ! -S /run/dbus/system_bus_socket ]; do
        sleep 0.05; _i=$((_i+1))
    done
fi

# Session bus with anonymous auth (works across proot instances)
export DBUS_SESSION_BUS_ADDRESS="unix:path=/tmp/dbus-session-bus-socket"
if ! dbus-send --session --dest=org.freedesktop.DBus /org/freedesktop/DBus org.freedesktop.DBus.GetId >/dev/null 2>&1; then
    rm -f /tmp/dbus-session-bus-socket
    dbus-daemon --config-file=/etc/dbus-1/proot-session.conf --nofork --nopidfile &
    _i=0; while [ "$_i" -lt 10 ] && [ ! -S /tmp/dbus-session-bus-socket ]; do
        sleep 0.05; _i=$((_i+1))
    done
fi

# Symlink Wayland and PipeWire sockets into the new XDG_RUNTIME_DIR
for _sock in wayland-0 pipewire-0; do
    [ -S "/tmp/$_sock" ] && [ ! -e "$XDG_RUNTIME_DIR/$_sock" ] && \
        ln -sf "/tmp/$_sock" "$XDG_RUNTIME_DIR/$_sock" 2>/dev/null
done
"#;
    if let Err(e) = fs::write(&start_dbus, script) {
        tracing::error!("[setup] Failed to write start-dbus: {}", e);
        return;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&start_dbus, fs::Permissions::from_mode(0o755));
    }
}

/// Create an empty system flatpak repo so `flatpak run` doesn't error
/// when checking /var/lib/flatpak/repo (we only use --user installs).
/// Needs a valid OSTree repo structure (config file + directories).
fn setup_flatpak_system_repo() {
    let repo_dir = Path::new(config::ARCH_FS_ROOT).join("var/lib/flatpak/repo");
    let config_file = repo_dir.join("config");
    if config_file.exists() {
        return;
    }
    setup_log("[setup] Creating empty flatpak system repo...");
    for subdir in ["objects", "refs/heads", "refs/mirrors", "refs/remotes", "tmp", "state"] {
        let _ = fs::create_dir_all(repo_dir.join(subdir));
    }
    let config = "[core]\nrepo_version=1\nmode=bare-user-only\n";
    fs::write(&config_file, config)
        .unwrap_or_else(|e| tracing::error!("[setup] Failed to write flatpak repo config: {}", e));
}

/// Install the XDG Desktop Portal Android backend.
///
/// Sets up:
/// 1. Portal descriptor file so xdg-desktop-portal knows about our backend
/// 2. D-Bus service file for auto-activation
/// 3. The Python backend script that bridges D-Bus ↔ compositor Unix socket
/// 4. Portal config to select our backend
pub fn setup_portal() {
    let fs_root = Path::new(config::ARCH_FS_ROOT);
    let backend_script = fs_root.join("usr/local/libexec/xdg-desktop-portal-android");

    // Re-generate if script doesn't exist or uses the old backend interface
    if backend_script.exists() {
        if let Ok(content) = fs::read_to_string(&backend_script) {
            if content.contains("apply_color_scheme") {
                return; // Already has the latest version with gsettings support
            }
        }
    }

    setup_log("[setup] Installing XDG Desktop Portal Android daemon...");

    // Remove conflicting service file if xdg-desktop-portal package left one behind
    let conflict = fs_root.join("usr/share/dbus-1/services/org.freedesktop.portal.Desktop.service");
    let _ = fs::remove_file(&conflict);

    // Standalone portal daemon — implements the frontend D-Bus interface directly
    // (started explicitly from launch.rs, no D-Bus auto-activation needed)
    let _ = fs::create_dir_all(fs_root.join("usr/local/libexec"));
    let script = r##"#!/usr/bin/env python3
"""
Standalone XDG Desktop Portal for Android.

Implements org.freedesktop.portal.FileChooser directly on the session bus,
bypassing xdg-desktop-portal (which needs /proc access that proot can't provide).
Forwards file chooser requests to the Android compositor via a Unix socket.
"""

import dbus
import dbus.service
import dbus.mainloop.glib
import json
import socket
import sys
import threading
from gi.repository import GLib

SOCKET_PATH = "/tmp/.portal-bridge"
BUS_NAME = "org.freedesktop.portal.Desktop"
OBJECT_PATH = "/org/freedesktop/portal/desktop"

request_counter = 0
main_loop = None


def send_portal_request(request):
    """Send a JSON request to the compositor and wait for response."""
    try:
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        sock.connect(SOCKET_PATH)
        sock.settimeout(300)
        sock.sendall((json.dumps(request) + "\n").encode())
        buf = b""
        while b"\n" not in buf:
            chunk = sock.recv(4096)
            if not chunk:
                break
            buf += chunk
        sock.close()
        if buf:
            return json.loads(buf.decode().strip())
    except Exception as e:
        print(f"Portal bridge error: {e}", file=sys.stderr)
    return {"response": 2, "uris": []}


class RequestObject(dbus.service.Object):
    """Represents a portal request. Emits Response signal when done."""

    def __init__(self, bus, path):
        super().__init__(bus, path)

    @dbus.service.signal("org.freedesktop.portal.Request", signature="ua{sv}")
    def Response(self, response, results):
        pass

    @dbus.service.method("org.freedesktop.portal.Request", in_signature="", out_signature="")
    def Close(self):
        self.remove_from_connection()


class PortalService(dbus.service.Object):
    """Implements org.freedesktop.portal.FileChooser (frontend interface)."""

    def __init__(self, bus, path):
        super().__init__(bus, path)
        self._bus = bus

    def _get_request_path(self, sender, options):
        global request_counter
        request_counter += 1
        token = str(options.get("handle_token", f"android{request_counter}"))
        sender_part = sender[1:].replace(".", "_")
        return f"/org/freedesktop/portal/desktop/request/{sender_part}/{token}"

    def _extract_mime_types(self, options):
        mime_types = []
        filters = options.get("filters", [])
        for f in filters:
            if len(f) >= 2:
                for pattern in f[1]:
                    if len(pattern) >= 2 and int(pattern[0]) == 1:
                        mime_types.append(str(pattern[1]))
        return mime_types or ["*/*"]

    @dbus.service.method(
        "org.freedesktop.portal.FileChooser",
        in_signature="ssa{sv}",
        out_signature="o",
        sender_keyword="sender",
    )
    def OpenFile(self, parent_window, title, options, sender=None):
        req_path = self._get_request_path(sender, options)
        req_obj = RequestObject(self._bus, req_path)
        multiple = bool(options.get("multiple", False))
        directory = bool(options.get("directory", False))
        mime_types = self._extract_mime_types(options)

        def do_request():
            result = send_portal_request({
                "type": "open_file",
                "id": req_path,
                "title": str(title),
                "multiple": multiple,
                "directory": directory,
                "mime_types": mime_types,
            })
            response = int(result.get("response", 2))
            uris = result.get("uris", [])
            results = {}
            if response == 0 and uris:
                results["uris"] = dbus.Array(uris, signature="s")
            GLib.idle_add(lambda: (req_obj.Response(dbus.UInt32(response), results),
                                   req_obj.remove_from_connection()))

        threading.Thread(target=do_request, daemon=True).start()
        return dbus.ObjectPath(req_path)

    @dbus.service.method(
        "org.freedesktop.portal.FileChooser",
        in_signature="ssa{sv}",
        out_signature="o",
        sender_keyword="sender",
    )
    def SaveFile(self, parent_window, title, options, sender=None):
        req_path = self._get_request_path(sender, options)
        req_obj = RequestObject(self._bus, req_path)
        current_name = str(options.get("current_name", ""))
        mime_types = self._extract_mime_types(options)

        def do_request():
            result = send_portal_request({
                "type": "save_file",
                "id": req_path,
                "title": str(title),
                "multiple": False,
                "directory": False,
                "mime_types": mime_types,
                "current_name": current_name,
            })
            response = int(result.get("response", 2))
            uris = result.get("uris", [])
            results = {}
            if response == 0 and uris:
                results["uris"] = dbus.Array(uris, signature="s")
            GLib.idle_add(lambda: (req_obj.Response(dbus.UInt32(response), results),
                                   req_obj.remove_from_connection()))

        threading.Thread(target=do_request, daemon=True).start()
        return dbus.ObjectPath(req_path)

    # Settings interface — exposes Android's color scheme to Linux apps

    def _get_color_scheme(self):
        """Query Android's color scheme via the compositor bridge."""
        result = send_portal_request({"type": "get_color_scheme"})
        return int(result.get("color_scheme", 0))

    @dbus.service.method(
        "org.freedesktop.portal.Settings",
        in_signature="ss",
        out_signature="v",
    )
    def ReadOne(self, namespace, key):
        if namespace == "org.freedesktop.appearance" and key == "color-scheme":
            return dbus.UInt32(self._get_color_scheme())
        raise dbus.exceptions.DBusException(
            f"Unknown setting: {namespace}.{key}",
            name="org.freedesktop.portal.Error.NotFound",
        )

    @dbus.service.method(
        "org.freedesktop.portal.Settings",
        in_signature="ss",
        out_signature="v",
    )
    def Read(self, namespace, key):
        # Deprecated method — wraps value in extra variant layer
        val = self.ReadOne(namespace, key)
        return dbus.types.Variant(val)

    @dbus.service.method(
        "org.freedesktop.portal.Settings",
        in_signature="as",
        out_signature="a{sa{sv}}",
    )
    def ReadAll(self, namespaces):
        result = {}
        # If no filter or matching filter, include appearance settings
        if not namespaces or any(
            ns in ("org.freedesktop.appearance", "org.freedesktop.*", "*")
            for ns in namespaces
        ):
            result["org.freedesktop.appearance"] = {
                "color-scheme": dbus.UInt32(self._get_color_scheme()),
            }
        return result

    @dbus.service.signal(
        "org.freedesktop.portal.Settings",
        signature="ssv",
    )
    def SettingChanged(self, namespace, key, value):
        pass

    # Properties interface — apps query portal versions
    @dbus.service.method(
        dbus.PROPERTIES_IFACE,
        in_signature="ss",
        out_signature="v",
    )
    def Get(self, interface, prop):
        if prop == "version":
            if interface == "org.freedesktop.portal.Settings":
                return dbus.UInt32(2)
            return dbus.UInt32(4)
        raise dbus.exceptions.DBusException(
            f"Unknown property: {interface}.{prop}",
            name="org.freedesktop.DBus.Error.UnknownProperty",
        )

    @dbus.service.method(
        dbus.PROPERTIES_IFACE,
        in_signature="s",
        out_signature="a{sv}",
    )
    def GetAll(self, interface):
        if interface == "org.freedesktop.portal.Settings":
            return {"version": dbus.UInt32(2)}
        return {"version": dbus.UInt32(4)}


def apply_color_scheme(scheme):
    """Apply color scheme to gsettings so GTK3/GTK4 apps update instantly."""
    import subprocess
    try:
        # GTK4 / GNOME 42+
        cs = {1: "prefer-dark", 2: "prefer-light"}.get(scheme, "default")
        subprocess.run(
            ["gsettings", "set", "org.gnome.desktop.interface", "color-scheme", cs],
            timeout=5, capture_output=True,
        )
        # GTK3 — theme name variant
        theme = "Adwaita-dark" if scheme == 1 else "Adwaita"
        subprocess.run(
            ["gsettings", "set", "org.gnome.desktop.interface", "gtk-theme", theme],
            timeout=5, capture_output=True,
        )
    except Exception as e:
        print(f"gsettings error: {e}", file=sys.stderr)


def poll_color_scheme(portal):
    """Poll Android color scheme and emit SettingChanged + update gsettings."""
    last_scheme = portal._get_color_scheme()
    apply_color_scheme(last_scheme)
    while True:
        import time
        time.sleep(5)
        try:
            scheme = portal._get_color_scheme()
            if scheme != last_scheme:
                last_scheme = scheme
                print(f"Color scheme changed to {scheme}", flush=True)
                apply_color_scheme(scheme)
                GLib.idle_add(
                    portal.SettingChanged,
                    "org.freedesktop.appearance",
                    "color-scheme",
                    dbus.UInt32(scheme),
                )
        except Exception as e:
            print(f"Poll error: {e}", file=sys.stderr)


def main():
    dbus.mainloop.glib.DBusGMainLoop(set_as_default=True)
    bus = dbus.SessionBus()
    bus_name = dbus.service.BusName(BUS_NAME, bus, replace_existing=True, allow_replacement=True)
    portal = PortalService(bus, OBJECT_PATH)
    # Poll for color scheme changes in background
    threading.Thread(target=poll_color_scheme, args=(portal,), daemon=True).start()
    print(f"Android portal running on {BUS_NAME}", flush=True)
    GLib.MainLoop().run()


if __name__ == "__main__":
    main()
"##;
    if let Err(e) = fs::write(&backend_script, script) {
        tracing::error!("[setup] Failed to write portal daemon: {e}");
        return;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&backend_script, fs::Permissions::from_mode(0o755));
    }
}

/// Fix the xkb symlink if it's absolute (won't resolve outside proot).
///
/// In Arch, `/usr/share/X11/xkb` is often an absolute symlink to
/// `/usr/share/xkeyboard-config-2`. Since libxkbcommon runs natively on
/// Android (not inside proot), absolute symlinks don't resolve. Convert
/// to a relative symlink.
pub fn fix_xkb_symlink() {
    let fs_root = Path::new(config::ARCH_FS_ROOT);
    let xkb_path = fs_root.join("usr/share/X11/xkb");

    let Ok(meta) = fs::symlink_metadata(&xkb_path) else {
        return;
    };
    if !meta.file_type().is_symlink() {
        return;
    }
    let Ok(target) = fs::read_link(&xkb_path) else {
        return;
    };
    if !target.is_absolute() {
        return;
    }

    setup_log(&format!(
        "[setup] Fixing absolute xkb symlink: {} -> {}",
        xkb_path.display(),
        target.display()
    ));

    // Compute relative path: both paths are inside the chroot
    let xkb_inside = Path::new("/usr/share/X11/xkb");
    let Some(xkb_parent) = xkb_inside.parent() else {
        return;
    };
    let rel_target = diff_paths(&target, xkb_parent).unwrap_or_else(|| target.clone());

    setup_log(&format!(
        "[setup] New relative symlink: {} -> {}",
        xkb_path.display(),
        rel_target.display()
    ));
    let _ = fs::remove_file(&xkb_path);
    if let Err(e) = symlink(&rel_target, &xkb_path) {
        tracing::error!("[setup] Failed to create xkb symlink: {}", e);
    }
}

/// Install the pre-built libhybris Vulkan ICD into the proot rootfs.
/// This enables glibc apps to use Android's proprietary GPU driver directly.
/// The .so files are cross-compiled on the host via ./build-libhybris.sh.
/// Idempotent: skips if already installed.
pub fn setup_hybris_vulkan() {
    let fs_root = Path::new(config::ARCH_FS_ROOT);
    let lib_dir = fs_root.join("usr/lib");
    let icd_dir = fs_root.join("usr/share/vulkan/icd.d");

    let icd_so = lib_dir.join("libvulkan_hybris.so");
    let hybris_so = lib_dir.join("libhybris-common.so");

    if icd_so.exists() && hybris_so.exists() {
        return;
    }

    setup_log("[setup] Installing hybris Vulkan ICD...");

    let _ = fs::create_dir_all(&lib_dir);
    let _ = fs::create_dir_all(&icd_dir);

    let linker_dir = fs_root.join("usr/lib/libhybris/linker");
    let _ = fs::create_dir_all(&linker_dir);

    // Pre-built binaries from ./build-libhybris.sh (cross-compiled on host)
    let files: &[(&str, &[u8])] = &[
        ("usr/lib/libhybris-common.so.1.0.0", include_bytes!("../../../libs/arm64-v8a-linux/libhybris-common.so.1.0.0")),
        ("usr/lib/libvulkan_hybris.so", include_bytes!("../../../libs/arm64-v8a-linux/libvulkan_hybris.so")),
        ("usr/share/vulkan/icd.d/hybris_vulkan_icd.json", include_bytes!("../../../hybris-vulkan-icd/hybris_vulkan_icd.json")),
        ("usr/lib/libhybris/linker/q.so", include_bytes!("../../../libs/arm64-v8a-linux/libhybris-linker/q.so")),
        ("usr/lib/libhybris/linker/o.so", include_bytes!("../../../libs/arm64-v8a-linux/libhybris-linker/o.so")),
        ("usr/lib/libhybris/linker/n.so", include_bytes!("../../../libs/arm64-v8a-linux/libhybris-linker/n.so")),
        ("usr/lib/libhybris/linker/mm.so", include_bytes!("../../../libs/arm64-v8a-linux/libhybris-linker/mm.so")),
    ];

    for (path, data) in files {
        let dest = fs_root.join(path);
        if let Err(e) = fs::write(&dest, data) {
            tracing::error!("[setup] Failed to write {}: {}", path, e);
            return;
        }
    }

    // Create soname symlinks
    let _ = std::fs::remove_file(lib_dir.join("libhybris-common.so.1"));
    let _ = std::fs::remove_file(lib_dir.join("libhybris-common.so"));
    let _ = symlink("libhybris-common.so.1.0.0", lib_dir.join("libhybris-common.so.1"));
    let _ = symlink("libhybris-common.so.1.0.0", lib_dir.join("libhybris-common.so"));

    setup_log("[setup] hybris Vulkan ICD installed");
}
