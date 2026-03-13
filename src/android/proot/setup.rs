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

// Optional UI logger callback — set by main.rs to forward logs to SetupActivity.
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
    ArchProcess {
        command: config::CHECK_CMD.to_string(),
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
    disable_bwrap();
    fix_xkb_symlink();
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
    let temp_file = context.data_dir.join("archlinux-fs.tar.xz");
    let extracted_dir = context.data_dir.join("archlinux-aarch64");

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
                            if let Err(err) = e.unpack_in(&context.data_dir) {
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
            command: config::CHECK_CMD.to_string(),
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
            command: config::INSTALL_CMD.to_string(),
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
