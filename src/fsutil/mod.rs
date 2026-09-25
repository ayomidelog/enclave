//! Host filesystem helpers: atomic writes and locks, mount inventory and the
//! raw mount syscalls, copy acceleration, ownership checks, and path containment.

mod copies;
mod creation;
mod mounts;
mod paths;
mod permissions;

pub use copies::{copy_file_range_file, reflink_copy_file};
#[cfg(test)]
pub(crate) use creation::CREATION_MARKER_NAME;
pub(crate) use creation::{creation_in_progress, remove_creation_marker, write_creation_marker};
pub use mounts::{bind_mount, is_mountpoint, make_mount_private};
pub(crate) use mounts::{enclave_state_root, MountInfoEntry, MountInfoSnapshot};
pub use paths::{canonicalize_within, ensure_path_within, slugify};
pub(crate) use permissions::temporary_path_for;
pub use permissions::{ensure_secure_dir, verify_secure_socket};

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};

const LOCK_TIMEOUT: Duration = Duration::from_secs(30);
const LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(25);

pub fn with_file_lock<T, F>(lock_path: &Path, operation: F) -> Result<T>
where
    F: FnOnce() -> Result<T>,
{
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let lock_file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .open(lock_path)
        .with_context(|| format!("failed to open lock file {}", lock_path.display()))?;

    let fd = lock_file.as_raw_fd();
    let started = std::time::Instant::now();
    loop {
        let lock_rc = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
        if lock_rc == 0 {
            break;
        }

        let err = std::io::Error::last_os_error();
        let err_no = err.raw_os_error().unwrap_or_default();
        let would_block = err_no == libc::EWOULDBLOCK || err_no == libc::EAGAIN;
        if would_block && started.elapsed() < LOCK_TIMEOUT {
            thread::sleep(LOCK_RETRY_INTERVAL);
            continue;
        }
        return Err(anyhow!("failed to lock {}: {}", lock_path.display(), err));
    }

    crate::perf::record_registry_lock_wait(started.elapsed().as_micros() as u64);
    let result = operation();

    let unlock_rc = unsafe { libc::flock(fd, libc::LOCK_UN) };
    let unlock_error = if unlock_rc != 0 {
        Some(anyhow!(
            "failed to unlock {}: {}",
            lock_path.display(),
            std::io::Error::last_os_error()
        ))
    } else {
        None
    };
    drop(lock_file);

    match (result, unlock_error) {
        (Ok(value), None) => Ok(value),
        (Err(err), None) => Err(err),
        (Ok(_), Some(unlock_err)) => Err(unlock_err),
        (Err(err), Some(unlock_err)) => Err(err.context(unlock_err.to_string())),
    }
}

/// Whether an atomic write has to survive a power loss before it returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Durability {
    /// fsync the file and its directory before returning.
    Required,
    /// Replace the file without fsync.
    ///
    /// The write is still atomic, so a reader never sees a partial file, but the
    /// rename may be lost in a power failure and the previous contents survive
    /// instead. That is the right trade for a progress note: the previous version
    /// is a truthful, earlier statement of the same thing, so losing the newest
    /// one costs a little detail rather than correctness.
    BestEffort,
}

pub fn write_file_atomic(path: &Path, content: &[u8], mode: u32) -> Result<()> {
    write_file_atomic_with(path, content, mode, Durability::Required)
}

pub fn write_file_atomic_with(
    path: &Path,
    content: &[u8],
    mode: u32,
    durability: Durability,
) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("path {} has no parent directory", path.display()))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create parent {}", parent.display()))?;

    let temp_path = temporary_path_for(path);
    let mut temp = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(mode)
        .open(&temp_path)
        .with_context(|| format!("failed to create temp file {}", temp_path.display()))?;
    temp.write_all(content)
        .with_context(|| format!("failed to write temp file {}", temp_path.display()))?;
    if durability == Durability::Required {
        temp.sync_all()
            .with_context(|| format!("failed to fsync temp file {}", temp_path.display()))?;
    }
    drop(temp);

    fs::set_permissions(&temp_path, fs::Permissions::from_mode(mode))
        .with_context(|| format!("failed to set mode on {}", temp_path.display()))?;
    if let Err(err) = fs::rename(&temp_path, path) {
        let cleanup_err = fs::remove_file(&temp_path).err();
        return Err(err).with_context(|| {
            let cleanup_note = cleanup_err
                .as_ref()
                .map(|e| {
                    format!(
                        "; failed to remove temp file {}: {}",
                        temp_path.display(),
                        e
                    )
                })
                .unwrap_or_default();
            format!(
                "failed to atomically rename {} -> {}{}",
                temp_path.display(),
                path.display(),
                cleanup_note
            )
        });
    }

    if durability == Durability::Required {
        let parent_file = OpenOptions::new()
            .read(true)
            .open(parent)
            .with_context(|| format!("failed to open directory {}", parent.display()))?;
        parent_file
            .sync_all()
            .with_context(|| format!("failed to fsync directory {}", parent.display()))?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "../../tests/src/fsutil.rs"]
mod tests;
