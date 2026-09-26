use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

const STATE_LOCK_FILE: &str = "daemon.lock";

/// How many times to retry when the lock file is replaced underneath us.
///
/// A daemon that exits removes the lock file, and one that is acquiring the lock
/// at that moment can end up holding a lock on the file that was just unlinked.
/// Each retry reopens the path, so it either locks the current file or reports
/// the owner of one that is still held.
const ACQUIRE_ATTEMPTS: usize = 4;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StateLockRecord {
    pub pid: u32,
    pub socket: String,
    pub binary_version: String,
    pub binary_path: String,
    pub started_at: String,
}

#[derive(Debug)]
pub(crate) struct StateLock {
    file: File,
    path: PathBuf,
    device: u64,
    inode: u64,
}

pub(crate) fn acquire_state_lock(state_dir: &Path, socket_path: &Path) -> Result<StateLock> {
    crate::fsutil::ensure_secure_dir(state_dir)?;
    let path = state_dir.join(STATE_LOCK_FILE);
    for _ in 0..ACQUIRE_ATTEMPTS {
        // A path that is not a regular file would either be followed to something
        // else (a symlink) or cannot hold an exclusion this daemon understands, and
        // `create(true)` cannot have made it one, so it is something already at the
        // path and is refused rather than replaced.
        match fs::symlink_metadata(&path) {
            Ok(metadata) if !metadata.is_file() => {
                return Err(anyhow!(
                    "daemon state lock {} is not a regular file; refusing to use it",
                    path.display()
                ))
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to stat daemon state lock {}", path.display())
                })
            }
        }
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .mode(0o600)
            .open(&path)
            .with_context(|| format!("failed to open daemon state lock {}", path.display()))?;
        let opened = file
            .metadata()
            .with_context(|| format!("failed to stat daemon state lock {}", path.display()))?;
        if !opened.is_file() {
            return Err(anyhow!(
                "daemon state lock {} is not a regular file; refusing to use it",
                path.display()
            ));
        }

        let lock_result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if lock_result != 0 {
            let owner =
                read_record_from_file(&mut file).unwrap_or_else(|| "unavailable".to_string());
            return Err(anyhow!(
                "state directory {} is already owned by another daemon (lock {}): {}",
                state_dir.display(),
                path.display(),
                owner
            ));
        }

        // The lock is on the inode, not on the path. A daemon that exited between
        // our open and our lock removed the file we just locked, so the path now
        // names either nothing or a different file, and this lock excludes nobody.
        // Retry so the next attempt locks whatever the path names now.
        let current = match fs::symlink_metadata(&path) {
            Ok(current) => current,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to stat daemon state lock {}", path.display())
                })
            }
        };
        if opened.dev() != current.dev() || opened.ino() != current.ino() {
            continue;
        }

        let record = StateLockRecord {
            pid: std::process::id(),
            socket: socket_path.to_string_lossy().to_string(),
            binary_version: env!("CARGO_PKG_VERSION").to_string(),
            binary_path: std::env::current_exe()
                .map(|path| path.to_string_lossy().to_string())
                .unwrap_or_else(|_| "unknown".to_string()),
            started_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        };
        let payload = serde_json::to_vec_pretty(&record)?;
        file.set_len(0)
            .with_context(|| format!("failed to truncate daemon state lock {}", path.display()))?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&payload)
            .with_context(|| format!("failed to write daemon state lock {}", path.display()))?;
        file.write_all(b"\n")?;
        file.sync_all()
            .with_context(|| format!("failed to sync daemon state lock {}", path.display()))?;

        return Ok(StateLock {
            file,
            path,
            device: opened.dev(),
            inode: opened.ino(),
        });
    }

    Err(anyhow!(
        "failed to acquire the daemon state lock {} after {ACQUIRE_ATTEMPTS} attempts; another daemon kept replacing it",
        path.display()
    ))
}

pub(crate) fn state_lock_path(state_dir: &Path) -> PathBuf {
    state_dir.join(STATE_LOCK_FILE)
}

pub(crate) fn read_state_lock_record(state_dir: &Path) -> Result<Option<StateLockRecord>> {
    let path = state_lock_path(state_dir);
    match fs::read_to_string(&path) {
        Ok(raw) => Ok(Some(serde_json::from_str(&raw).with_context(|| {
            format!("invalid daemon state lock record {}", path.display())
        })?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
}

impl Drop for StateLock {
    fn drop(&mut self) {
        // Remove the file only while it is still the one this daemon locked, and
        // before releasing the lock. Removing a lock file that another daemon has
        // already replaced would delete its exclusion and let a third daemon start
        // against the same state directory.
        if fs::symlink_metadata(&self.path)
            .is_ok_and(|current| current.dev() == self.device && current.ino() == self.inode)
        {
            let _ = fs::remove_file(&self.path);
        }
        let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

fn read_record_from_file(file: &mut File) -> Option<String> {
    file.seek(SeekFrom::Start(0)).ok()?;
    let mut raw = String::new();
    file.read_to_string(&mut raw).ok()?;
    let record: StateLockRecord = serde_json::from_str(&raw).ok()?;
    Some(format!(
        "pid={} socket={} version={} started_at={}",
        record.pid, record.socket, record.binary_version, record.started_at
    ))
}

#[cfg(test)]
#[path = "../../tests/src/daemon/state_lock.rs"]
mod tests;
