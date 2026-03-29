//! Helpers for creating Unix sockets inside the proot rootfs.

use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::Path;

/// Remove any stale socket, ensure the parent directory exists, bind a new
/// [`UnixListener`], and set the socket to mode `0o777` so proot clients can
/// connect.
pub fn create_unix_listener(path: &Path) -> std::io::Result<UnixListener> {
    prepare_socket_path(path)?;
    let listener = UnixListener::bind(path)?;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o777));
    Ok(listener)
}

/// Prepare a filesystem path for socket binding: remove any stale socket file
/// and create the parent directory if it does not exist.
pub fn prepare_socket_path(path: &Path) -> std::io::Result<()> {
    let _ = std::fs::remove_file(path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}
