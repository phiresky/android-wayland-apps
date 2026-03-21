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
    /// When false, omit --kill-on-exit so forked daemons survive.
    pub kill_on_exit: bool,
}

impl ArchProcess {
    fn probe_proot(no_seccomp: bool) -> bool {
        let context = get_application_context();
        let proot_loader = context.native_library_dir.join("libproot_loader.so");

        let mut process = Command::new(context.native_library_dir.join("libproot.so"));
        process
            .env("PROOT_LOADER", &proot_loader)
            .env("PROOT_TMP_DIR", context.cache_dir.join("proot"));

        if no_seccomp {
            process.env("PROOT_NO_SECCOMP", "1");
        }

        process
            .arg("-V")
            .output()
            .map(|o| {
                tracing::info!(
                    "probe_proot(no_seccomp={}) {:?}, stdout: {}, stderr: {}",
                    no_seccomp,
                    o.status.code(),
                    String::from_utf8_lossy(&o.stdout),
                    String::from_utf8_lossy(&o.stderr)
                );
                o.status.success()
            })
            .unwrap_or_else(|e| {
                tracing::info!("probe_proot(no_seccomp={}) error: {}", no_seccomp, e);
                false
            })
    }

    pub fn is_supported() -> bool {
        let supported = if Self::probe_proot(false) {
            USE_NO_SECCOMP.set(false).ok();
            tracing::info!("PRoot works with seccomp filter enabled");
            true
        } else if Self::probe_proot(true) {
            USE_NO_SECCOMP.set(true).ok();
            tracing::info!("PRoot works with PROOT_NO_SECCOMP=1");
            true
        } else {
            USE_NO_SECCOMP.set(false).ok();
            false
        };

        if !supported {
            tracing::error!("Device Unsupported");
        }
        supported
    }

    pub fn run(self) -> Output {
        let context = get_application_context();
        let user = self.user.as_deref().unwrap_or("root");

        let proot_tmp = context.cache_dir.join("proot");
        let _ = std::fs::create_dir_all(&proot_tmp);

        let mut process = Command::new(context.native_library_dir.join("libproot.so"));
        process
            .env(
                "PROOT_LOADER",
                context.native_library_dir.join("libproot_loader.so"),
            )
            .env("PROOT_TMP_DIR", &proot_tmp);

        if *USE_NO_SECCOMP.get().unwrap_or(&false) {
            process.env("PROOT_NO_SECCOMP", "1");
        }

        process
            .arg("-r")
            .arg(config::ARCH_FS_ROOT)
            .arg("-L")
            .arg("--link2symlink")
            .arg("--sysvipc");
        if self.kill_on_exit {
            process.arg("--kill-on-exit");
        }
        process.arg("--root-id")
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
            .arg(format!("--bind={}/sys/.empty:/sys/fs/selinux", config::ARCH_FS_ROOT))
            // Expose the native lib dir and Android system libs inside proot so
            // nested proot can be invoked (used by the bwrap shim for flatpak)
            .arg(format!("--bind={}:{}", context.native_library_dir.display(), context.native_library_dir.display()))
            .arg("--bind=/system:/system")
            .arg("--bind=/apex:/apex");

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

        // Proot paths for nested proot (bwrap shim uses these for flatpak)
        process
            .arg(format!("_PROOT_BIN={}", context.native_library_dir.join("libproot.so").display()))
            .arg(format!("_PROOT_LOADER={}", context.native_library_dir.join("libproot_loader.so").display()))
            .arg(format!("_PROOT_TMP_DIR={}", context.cache_dir.join("proot").display()));

        // Wayland environment variables
        process
            .arg(format!("WAYLAND_DISPLAY={}", config::WAYLAND_SOCKET_NAME))
            .arg("XDG_RUNTIME_DIR=/tmp")
            .arg("QT_QPA_PLATFORM=wayland")
            .arg("GTK_OVERLAY_SCROLLING=0")
            .arg("TERM=xterm-256color")
            .arg("SHELL=/bin/bash")
            // Firefox sandbox uses seccomp which conflicts with proot's own seccomp
            .arg("MOZ_DISABLE_CONTENT_SANDBOX=1")
            .arg("MOZ_DISABLE_GMP_SANDBOX=1")
            .arg("MOZ_DISABLE_SOCKET_PROCESS_SANDBOX=1");

        // LD_PRELOAD: fix_ttyname shim if present
        let fix_ttyname = Path::new(config::ARCH_FS_ROOT).join("usr/lib/fix_ttyname.so");
        if fix_ttyname.exists() {
            process.arg("LD_PRELOAD=/usr/lib/fix_ttyname.so");
        }

        let wrapped_command = self.command.clone();

        // --root-id fakes UID 0 for proot internals (bind mounts etc.).
        // For non-root users, drop to the target user via `su` so the
        // process actually runs with the right uid/gid and $HOME.
        if user == "root" {
            process.arg("sh").arg("-c").arg(&wrapped_command);
        } else {
            // su (without -l) changes uid/gid but preserves the environment.
            // We already set HOME/USER/PATH above via env -i, so we just need
            // the uid change. proot's seccomp intercepts setuid to make this work.
            process.arg("su").arg(user).arg("-c").arg(&wrapped_command);
        }

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
                    tracing::error!("Failed to spawn proot command: {}", e);
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
                                tracing::error!("Error reading proot stderr: {}", e);
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
                            tracing::error!("Error reading proot stdout: {}", e);
                            break;
                        }
                    }
                }
            }

            if let Some(t) = stderr_thread {
                if let Err(e) = t.join() {
                    tracing::error!("stderr reader thread panicked: {:?}", e);
                }
            }

            match child.wait_with_output() {
                Ok(output) => output,
                Err(e) => {
                    tracing::error!("Failed to wait for proot command: {}", e);
                    failed()
                }
            }
        } else {
            match process.output() {
                Ok(output) => output,
                Err(e) => {
                    tracing::error!("Failed to run proot command: {}", e);
                    failed()
                }
            }
        }
    }
}
