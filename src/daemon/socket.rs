//! The daemon socket and pid file lifecycle.
//!
//! The socket path is what a client connects to and what proves whether a daemon
//! is already running, so preparing it is the step that has to be careful: a live
//! socket means a live daemon, a stale socket is safe to replace, and anything
//! that is not a socket at all is refused rather than removed.

use std::fs;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::UnixStream;
use std::path::Path;

use anyhow::{bail, Context, Result};

pub(super) fn prepare_runtime_paths(socket_path: &Path, pid_file: &Path) -> Result<()> {
    if let Some(parent) = socket_path.parent() {
        crate::fsutil::ensure_secure_dir(parent)?;
    }
    if let Some(parent) = pid_file.parent() {
        crate::fsutil::ensure_secure_dir(parent)?;
    }
    if socket_path.exists() {
        let metadata = fs::symlink_metadata(socket_path)
            .with_context(|| format!("failed to stat {}", socket_path.display()))?;
        if metadata.file_type().is_symlink() {
            bail!(
                "refusing to use symlink socket path {}",
                socket_path.display()
            );
        }
        if !metadata.file_type().is_socket() {
            bail!(
                "refusing to use non-socket path {} for daemon socket",
                socket_path.display()
            );
        }

        match UnixStream::connect(socket_path) {
            Ok(_) => {
                bail!(
                    "socket path {} is active; stop the running daemon first",
                    socket_path.display()
                );
            }
            Err(err) if err.kind() == std::io::ErrorKind::ConnectionRefused => {
                fs::remove_file(socket_path).with_context(|| {
                    format!("failed to remove stale socket {}", socket_path.display())
                })?;
            }
            Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
                bail!(
                    "permission denied while probing existing socket {}: {}",
                    socket_path.display(),
                    err
                );
            }
            Err(err) => {
                bail!(
                    "failed to probe existing socket {}: {}",
                    socket_path.display(),
                    err
                );
            }
        }
    }
    Ok(())
}

pub(super) fn cleanup_files(socket_path: &Path, pid_file: &Path) -> Result<()> {
    if socket_path.exists() {
        fs::remove_file(socket_path)
            .with_context(|| format!("failed to remove socket {}", socket_path.display()))?;
    }
    if pid_file.exists() {
        fs::remove_file(pid_file)
            .with_context(|| format!("failed to remove pid file {}", pid_file.display()))?;
    }
    Ok(())
}
