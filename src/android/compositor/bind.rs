use crate::core::config;
use smithay::reexports::wayland_server::ListeningSocket;
use std::{error::Error, path::PathBuf};

pub fn bind_socket() -> Result<ListeningSocket, Box<dyn Error>> {
    let socket_dir = PathBuf::from(config::ARCH_FS_ROOT.to_owned() + "/tmp");
    std::fs::create_dir_all(&socket_dir)?;
    let socket_path = socket_dir.join(config::WAYLAND_SOCKET_NAME);
    // Remove stale socket if it exists from a previous run.
    let _ = std::fs::remove_file(&socket_path);
    let listener = ListeningSocket::bind_absolute(socket_path)?;
    Ok(listener)
}
