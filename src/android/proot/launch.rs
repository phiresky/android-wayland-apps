use super::process::ArchProcess;
use crate::core::config;
use std::sync::Arc;
use std::thread;

pub fn launch() {
    thread::spawn(move || {
        ArchProcess {
            command: config::default_launch_command(),
            user: None,
            log: Some(Arc::new(|it| log::trace!("{}", it))),
        }
        .run();
    });
}
