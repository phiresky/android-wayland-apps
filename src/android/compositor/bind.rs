use crate::android::utils::socket::prepare_socket_path;
use crate::core::config;
use smithay::reexports::wayland_server::ListeningSocket;
use std::{error::Error, path::PathBuf};

pub fn bind_socket() -> Result<ListeningSocket, Box<dyn Error>> {
    let socket_path =
        PathBuf::from(config::ARCH_FS_ROOT.to_owned() + "/tmp").join(config::WAYLAND_SOCKET_NAME);
    prepare_socket_path(&socket_path)?;
    let listener = ListeningSocket::bind_absolute(socket_path)?;
    Ok(listener)
}
