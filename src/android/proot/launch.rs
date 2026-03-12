use super::process::ArchProcess;
use crate::core::config;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

pub fn launch() {
    let local_config =
        config::parse_config(format!("{}{}", config::ARCH_FS_ROOT, config::CONFIG_FILE));
    let username = local_config.user.username;
    let command = local_config.command.launch;

    thread::spawn(move || {
        log::info!("Launching: {}", command);
        let output = ArchProcess {
            command: command.clone(),
            user: Some(username),
            log: Some(Arc::new(|it| log::info!("{}", it))),
        }
        .run();
        log::info!("Launch command exited: {:?}", output.status);
    });

    // Launch demo apps after a short delay to let the terminal connect first.
    for (delay_ms, cmd) in [(1500, "eglgears_wayland"), (2000, "gedit")] {
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(delay_ms));
            log::info!("Launching: {}", cmd);
            let output = ArchProcess {
                command: cmd.to_string(),
                user: Some("root".to_string()),
                log: Some(Arc::new(|it| log::info!("{}", it))),
            }
            .run();
            log::info!("{} exited: {:?}", cmd, output.status);
        });
    }
}
