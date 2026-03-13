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
    log::info!("{}", msg);
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
    setup_dns();
    install_dependencies();
    setup_user();
    disable_bwrap();
    fix_bsdtar();
    fix_xkb_symlink();
    fix_ttyname();
    setup_storage_mountpoints();
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
            .unwrap_or_else(|e| log::error!("[setup] Failed to write {}: {}", path, e));
    }
}

/// Ensure resolv.conf exists with a working nameserver.
/// glibc inside proot needs this for DNS resolution.
fn setup_dns() {
    let resolv_conf = Path::new(config::ARCH_FS_ROOT).join("etc/resolv.conf");
    if resolv_conf.exists() {
        return;
    }
    setup_log("[setup] Writing resolv.conf...");
    if let Some(parent) = resolv_conf.parent() {
        let _ = fs::create_dir_all(parent);
    }
    fs::write(&resolv_conf, "nameserver 8.8.8.8\n")
        .unwrap_or_else(|e| log::error!("[setup] Failed to write resolv.conf: {}", e));
}

/// Install dependencies via pacman if the check command fails.
fn install_dependencies() {
    let is_installed = || {
        ArchProcess {
            command: config::check_cmd(),
            user: None,
            log: None,
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
        }
        .run();

        // Run install command with output logged to logcat and UI
        ArchProcess {
            command: config::install_cmd(),
            user: None,
            log: Some(Arc::new(|line| setup_log(&format!("[pacman] {}", line)))),
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
    .unwrap_or_else(|e| log::error!("[setup] Failed to write sudoers for {}: {}", user, e));
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
            log::error!("[setup] Failed to rename bwrap: {}", e);
            return;
        }
    }

    if !bwrap_real.exists() {
        return;
    }

    let shim = r#"#!/bin/sh
# bwrap shim: runs the command unsandboxed (proot can't do namespaces)
while [ $# -gt 0 ]; do
    case "$1" in
        --setenv) export "$2=$3"; shift 3 ;;
        --unshare-all|--die-with-parent|--clearenv|--new-session) shift ;;
        --chdir|--dev|--tmpfs|--proc|--seccomp|--userns|--userns2) shift 2 ;;
        --ro-bind|--bind|--symlink|--ro-bind-try|--bind-try|--dev-bind|--dev-bind-try) shift 3 ;;
        --) shift; exec "$@" ;;
        /*) exec "$@" ;;
        *) shift ;;
    esac
done
"#;

    setup_log("[setup] Installing bwrap shim (sandboxing incompatible with proot)");
    if let Err(e) = fs::write(&bwrap, shim) {
        log::error!("[setup] Failed to write bwrap shim: {}", e);
        return;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&bwrap, fs::Permissions::from_mode(0o755));
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
        log::error!("[setup] Failed to write bsdtar wrapper: {}", e);
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
        log::error!("[setup] Failed to write fix_ttyname.c: {}", e);
        return;
    }

    let output = ArchProcess {
        command: "gcc -shared -fPIC -o /usr/lib/fix_ttyname.so /tmp/fix_ttyname.c && echo OK"
            .into(),
        user: None,
        log: None,
    }
    .run();

    if output.status.success()
        && String::from_utf8_lossy(&output.stdout).contains("OK")
    {
        setup_log("[setup] ttyname fix built successfully");
    } else {
        log::error!(
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
                log::error!("[setup] Failed to create /{}: {}", dir, e);
            }
        }
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
        log::error!("[setup] Failed to create xkb symlink: {}", e);
    }
}
