use super::process::ArchProcess;
use crate::core::config;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

pub fn launch() {
    thread::spawn(move || {
        log::info!("Launching: {}", config::LAUNCH_CMD);
        let output = ArchProcess {
            command: config::LAUNCH_CMD.to_string(),
            user: Some(config::DEFAULT_USERNAME.to_string()),
            log: Some(Arc::new(|it| log::info!("{}", it))),
        }
        .run();
        log::info!("Launch command exited: {:?}", output.status);
    });

    // Launch demo apps after a short delay to let the terminal connect first.
    for (delay_ms, cmd) in [(1500, "weston-simple-egl"), (2000, "gedit")] {
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
