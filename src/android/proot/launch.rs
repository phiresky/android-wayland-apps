use super::process::ArchProcess;
use crate::core::config;
use std::sync::Arc;
use std::thread;

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
}
