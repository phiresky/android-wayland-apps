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

        // Load shared proot config (binds + env vars) from proot-config.json.
        let cfg: serde_json::Value = serde_json::from_str(
            include_str!("../../../proot-config.json")
        ).unwrap_or_else(|e| panic!("bad proot-config.json: {e}"));

        let rootfs = config::ARCH_FS_ROOT;
        let libdir = context.native_library_dir.to_string_lossy();
        let cache_dir = context.cache_dir.to_string_lossy();
        let subst = |s: &str| -> String {
            s.replace("$ROOTFS", rootfs)
             .replace("$LIBDIR", &libdir)
             .replace("$CACHE_DIR", &cache_dir)
        };

        // proot flags
        process.arg("-r").arg(rootfs);
        for arg in cfg["proot_args"].as_array().into_iter().flatten() {
            if let Some(s) = arg.as_str() { process.arg(s); }
        }
        if self.kill_on_exit {
            process.arg("--kill-on-exit");
        }

        // binds (always)
        for bind in cfg["binds"].as_array().into_iter().flatten() {
            if let Some(s) = bind.as_str() {
                process.arg(format!("--bind={}", subst(s)));
            }
        }
        // binds (only if source path exists)
        for bind in cfg["binds_if_exists"].as_array().into_iter().flatten() {
            if let Some(s) = bind.as_str() {
                let src = subst(s.split(':').next().unwrap_or(s));
                if Path::new(&src).exists() {
                    process.arg(format!("--bind={}", subst(s)));
                }
            }
        }

        // env vars
        process.arg("/usr/bin/env").arg("-i");
        if user == "root" {
            process.arg("HOME=/root");
        } else {
            process.arg(format!("HOME=/home/{}", user));
        }
        process
            .arg(format!("USER={}", user))
            .arg(format!("LOGNAME={}", user));

        for (key, val) in cfg["env"].as_object().into_iter().flatten() {
            if let Some(v) = val.as_str() {
                process.arg(format!("{}={}", key, subst(v)));
            }
        }

        // GPU-conditional env vars (Zink needs a working Vulkan ICD in proot)
        let has_gpu = Path::new("/dev/kgsl-3d0").exists();
        if has_gpu {
            for (key, val) in cfg["env_if_gpu"].as_object().into_iter().flatten() {
                if let Some(v) = val.as_str() {
                    process.arg(format!("{}={}", key, v));
                }
            }
        }

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
