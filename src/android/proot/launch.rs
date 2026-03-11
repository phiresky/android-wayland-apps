use super::process::ArchProcess;
use crate::core::config;
use std::sync::Arc;
use std::thread;

pub fn launch() {
    let local_config =
        config::parse_config(format!("{}{}", config::ARCH_FS_ROOT, config::CONFIG_FILE));
    let username = local_config.user.username;
    let command = local_config.command.launch;

    // Launch the configured command (weston-terminal)
    let user1 = username.clone();
    let cmd1 = command.clone();
    thread::spawn(move || {
        log::info!("Launching: {}", cmd1);
        let output = ArchProcess {
            command: cmd1,
            user: Some(user1),
            log: Some(Arc::new(|it| log::info!("{}", it))),
        }
        .run();
        log::info!("weston-terminal exited: {:?}", output.status);
    });

    // Launch eglgears_wayland
    let user2 = username.clone();
    thread::spawn(move || {
        thread::sleep(std::time::Duration::from_secs(2));
        log::info!("Launching: eglgears_wayland");
        let output = ArchProcess {
            command: "eglgears_wayland".to_string(),
            user: Some(user2),
            log: Some(Arc::new(|it| log::info!("[eglgears] {}", it))),
        }
        .run();
        log::info!("eglgears exited: {:?}, stderr: {}", output.status, String::from_utf8_lossy(&output.stderr));
    });

    // Launch gedit
    let user3 = username.clone();
    thread::spawn(move || {
        thread::sleep(std::time::Duration::from_secs(3));
        log::info!("Launching: gedit");
        let output = ArchProcess {
            command: "gedit".to_string(),
            user: Some(user3),
            log: Some(Arc::new(|it| log::info!("[gedit] {}", it))),
        }
        .run();
        log::info!("gedit exited: {:?}, stderr: {}", output.status, String::from_utf8_lossy(&output.stderr));
    });

    // Launch weston-flower (simple demo, bundled with weston)
    thread::spawn(move || {
        thread::sleep(std::time::Duration::from_secs(4));
        log::info!("Launching: weston-flower");
        let output = ArchProcess {
            command: "weston-flower".to_string(),
            user: Some(username),
            log: Some(Arc::new(|it| log::info!("[flower] {}", it))),
        }
        .run();
        log::info!("weston-flower exited: {:?}, stderr: {}", output.status, String::from_utf8_lossy(&output.stderr));
    });
}
