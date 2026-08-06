use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

const STATE_LOCK_FILE: &str = "daemon.lock";

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
    _file: File,
    path: PathBuf,
}

pub(crate) fn acquire_state_lock(state_dir: &Path, socket_path: &Path) -> Result<StateLock> {
    crate::fsutil::ensure_secure_dir(state_dir)?;
    let path = state_dir.join(STATE_LOCK_FILE);
    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .open(&path)
        .with_context(|| format!("failed to open daemon state lock {}", path.display()))?;

    let lock_result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if lock_result != 0 {
        let owner = read_record_from_file(&mut file).unwrap_or_else(|| "unavailable".to_string());
        return Err(anyhow!(
            "state directory {} is already owned by another daemon (lock {}): {}",
            state_dir.display(),
            path.display(),
            owner
        ));
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

    Ok(StateLock { _file: file, path })
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
        let _ = unsafe { libc::flock(self._file.as_raw_fd(), libc::LOCK_UN) };
        let _ = fs::remove_file(&self.path);
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
