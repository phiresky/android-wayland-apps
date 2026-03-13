use crate::android::utils::application_context::get_application_context;
use crate::core::config;
use std::io::{BufRead, BufReader};
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Command, ExitStatus, Output, Stdio};
use std::sync::{Arc, OnceLock};

static USE_NO_SECCOMP: OnceLock<bool> = OnceLock::new();

pub type Log = Arc<dyn Fn(String) + Send + Sync>;

/// Runs a shell command inside the Arch Linux PRoot environment.
///
/// - `command`: The shell command to execute (passed to `sh -c`).
/// - `user`: The user to run as. Defaults to `"root"` when `None`.
/// - `log`: Optional stdout line callback. When set, stdout is streamed line-by-line
///   to the callback. When `None`, stdout/stderr are captured.
pub struct ArchProcess {
    pub command: String,
    pub user: Option<String>,
    pub log: Option<Log>,
}

impl ArchProcess {
    fn probe_proot(no_seccomp: bool) -> bool {
        let context = get_application_context();
        let proot_loader = context.native_library_dir.join("libproot_loader.so");

        let mut process = Command::new(context.native_library_dir.join("libproot.so"));
        process
            .env("PROOT_LOADER", &proot_loader)
            .env("PROOT_TMP_DIR", &context.data_dir);

        if no_seccomp {
            process.env("PROOT_NO_SECCOMP", "1");
        }

        process
            .arg("-V")
            .output()
            .map(|o| {
                log::info!(
                    "probe_proot(no_seccomp={}) {:?}, stdout: {}, stderr: {}",
                    no_seccomp,
                    o.status.code(),
                    String::from_utf8_lossy(&o.stdout),
                    String::from_utf8_lossy(&o.stderr)
                );
                o.status.success()
            })
            .unwrap_or_else(|e| {
                log::info!("probe_proot(no_seccomp={}) error: {}", no_seccomp, e);
                false
            })
    }

    pub fn is_supported() -> bool {
        let supported = if Self::probe_proot(false) {
            USE_NO_SECCOMP.set(false).ok();
            log::info!("PRoot works with seccomp filter enabled");
            true
        } else if Self::probe_proot(true) {
            USE_NO_SECCOMP.set(true).ok();
            log::info!("PRoot works with PROOT_NO_SECCOMP=1");
            true
        } else {
            USE_NO_SECCOMP.set(false).ok();
            false
        };

        if !supported {
            log::error!("Device Unsupported");
        }
        supported
    }

    pub fn run(self) -> Output {
        let context = get_application_context();
        let user = self.user.as_deref().unwrap_or("root");

        let mut process = Command::new(context.native_library_dir.join("libproot.so"));
        process
            .env(
                "PROOT_LOADER",
                context.native_library_dir.join("libproot_loader.so"),
            )
            .env("PROOT_TMP_DIR", context.data_dir);

        if *USE_NO_SECCOMP.get().unwrap_or(&false) {
            process.env("PROOT_NO_SECCOMP", "1");
        }

        process
            .arg("-r")
            .arg(config::ARCH_FS_ROOT)
            .arg("-L")
            .arg("--link2symlink")
            .arg("--sysvipc")
            .arg("--kill-on-exit")
            .arg("--root-id")
            .arg("--bind=/dev")
            .arg("--bind=/proc")
            .arg("--bind=/sys")
            .arg(format!("--bind={}/tmp:/dev/shm", config::ARCH_FS_ROOT));

        // Only bind external storage if it's accessible (requires MANAGE_EXTERNAL_STORAGE).
        // If not granted, proot would fail trying to bind an inaccessible FUSE mount.
        if Path::new("/storage/emulated/0").exists() {
            process
                .arg("--bind=/storage/emulated/0:/storage/emulated/0")
                .arg("--bind=/storage/emulated/0:/sdcard");
        }

        process
            .arg("--bind=/dev/urandom:/dev/random")
            .arg("--bind=/proc/self/fd:/dev/fd")
            .arg("--bind=/proc/self/fd/0:/dev/stdin")
            .arg("--bind=/proc/self/fd/1:/dev/stdout")
            .arg("--bind=/proc/self/fd/2:/dev/stderr")
            .arg(format!("--bind={}/proc/.loadavg:/proc/loadavg", config::ARCH_FS_ROOT))
            .arg(format!("--bind={}/proc/.stat:/proc/stat", config::ARCH_FS_ROOT))
            .arg(format!("--bind={}/proc/.uptime:/proc/uptime", config::ARCH_FS_ROOT))
            .arg(format!("--bind={}/proc/.version:/proc/version", config::ARCH_FS_ROOT))
            .arg(format!("--bind={}/proc/.vmstat:/proc/vmstat", config::ARCH_FS_ROOT))
            .arg(format!("--bind={}/proc/.sysctl_entry_cap_last_cap:/proc/sys/kernel/cap_last_cap", config::ARCH_FS_ROOT))
            .arg(format!("--bind={}/proc/.sysctl_inotify_max_user_watches:/proc/sys/fs/inotify/max_user_watches", config::ARCH_FS_ROOT))
            .arg(format!("--bind={}/sys/.empty:/sys/fs/selinux", config::ARCH_FS_ROOT));

        // env vars
        process.arg("/usr/bin/env").arg("-i");
        if user == "root" {
            process.arg("HOME=/root");
        } else {
            process.arg(format!("HOME=/home/{}", user));
        }
        process
            .arg("LANG=C.UTF-8")
            .arg("PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/usr/local/games:/usr/games:/system/bin:/system/xbin")
            .arg("TMPDIR=/tmp")
            .arg(format!("USER={}", user))
            .arg(format!("LOGNAME={}", user));

        // Wayland environment variables
        process
            .arg(format!("WAYLAND_DISPLAY={}", config::WAYLAND_SOCKET_NAME))
            .arg("XDG_RUNTIME_DIR=/tmp")
            .arg("QT_QPA_PLATFORM=wayland")
            .arg("TERM=xterm-256color")
            .arg("SHELL=/bin/bash");

        // Work around Android SELinux blocking readdir on /dev/pts (see setup.rs fix_ttyname)
        let fix_ttyname = Path::new(config::ARCH_FS_ROOT).join("usr/lib/fix_ttyname.so");
        if fix_ttyname.exists() {
            process.arg("LD_PRELOAD=/usr/lib/fix_ttyname.so");
        }

        // Run sh directly — --root-id already virtualizes the UID inside proot,
        // and USER/HOME are set above. runuser/su would call setuid() which fails
        // with PROOT_NO_SECCOMP=1 since the kernel rejects it from a non-root process.
        process.arg("sh").arg("-c").arg(&self.command);

        let failed = || Output {
            status: ExitStatus::from_raw(1),
            stdout: Vec::new(),
            stderr: Vec::new(),
        };

        if let Some(log) = self.log {
            let mut child = match process
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
            {
                Ok(c) => c,
                Err(e) => {
                    log::error!("Failed to spawn proot command: {}", e);
                    return failed();
                }
            };

            let stderr_log = log.clone();
            let stderr_thread = child.stderr.take().map(|stderr| {
                std::thread::spawn(move || {
                    let reader = BufReader::new(stderr);
                    for line in reader.lines() {
                        match line {
                            Ok(l) => stderr_log(format!("[stderr] {}", l)),
                            Err(e) => {
                                log::error!("Error reading proot stderr: {}", e);
                                break;
                            }
                        }
                    }
                })
            });

            if let Some(stdout) = child.stdout.take() {
                let reader = BufReader::new(stdout);
                for line in reader.lines() {
                    match line {
                        Ok(l) => log(l),
                        Err(e) => {
                            log::error!("Error reading proot stdout: {}", e);
                            break;
                        }
                    }
                }
            }

            if let Some(t) = stderr_thread {
                if let Err(e) = t.join() {
                    log::error!("stderr reader thread panicked: {:?}", e);
                }
            }

            match child.wait_with_output() {
                Ok(output) => output,
                Err(e) => {
                    log::error!("Failed to wait for proot command: {}", e);
                    failed()
                }
            }
        } else {
            match process.output() {
                Ok(output) => output,
                Err(e) => {
                    log::error!("Failed to run proot command: {}", e);
                    failed()
                }
            }
        }
    }
}
