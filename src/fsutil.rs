use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use uuid::Uuid;

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

pub fn write_file_atomic(path: &Path, content: &[u8], mode: u32) -> Result<()> {
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
    temp.sync_all()
        .with_context(|| format!("failed to fsync temp file {}", temp_path.display()))?;
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

    let parent_file = OpenOptions::new()
        .read(true)
        .open(parent)
        .with_context(|| format!("failed to open directory {}", parent.display()))?;
    parent_file
        .sync_all()
        .with_context(|| format!("failed to fsync directory {}", parent.display()))?;
    Ok(())
}

pub fn ensure_secure_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))?;

    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("failed to stat {}", path.display()))?;
    if !metadata.is_dir() {
        bail!("{} is not a directory", path.display());
    }
    if metadata.file_type().is_symlink() {
        bail!("{} must not be a symlink", path.display());
    }

    let euid = current_euid();
    if metadata.uid() != euid {
        bail!(
            "{} owner uid {} does not match current uid {}",
            path.display(),
            metadata.uid(),
            euid
        );
    }

    let mode = metadata.mode() & 0o7777;
    let is_world_writable_sticky = (mode & 0o1000 != 0) && (mode & 0o002 != 0);
    if is_world_writable_sticky {
        bail!(
            "{} has sticky bit set and is world-writable (mode {:o}); use a private subdirectory (0700) instead",
            path.display(),
            mode
        );
    }
    if mode & 0o022 != 0 {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("failed to tighten permissions on {}", path.display()))?;
    }
    Ok(())
}

pub fn verify_secure_socket(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to stat socket {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        bail!("socket path {} must not be a symlink", path.display());
    }
    if !metadata.file_type().is_socket() {
        bail!("path {} is not a unix socket", path.display());
    }

    let euid = current_euid();
    if metadata.uid() != euid {
        bail!(
            "socket {} is owned by uid {}, expected {}",
            path.display(),
            metadata.uid(),
            euid
        );
    }

    let mode = metadata.mode() & 0o777;
    if mode & 0o022 != 0 {
        bail!(
            "socket {} is group/world writable (mode {:o}); expected owner-only access",
            path.display(),
            mode
        );
    }
    Ok(())
}

pub fn canonicalize_within(base_dir: &Path, candidate: &Path, label: &str) -> Result<PathBuf> {
    ensure_path_within(base_dir, candidate, label)
}

pub fn ensure_path_within(base_dir: &Path, candidate: &Path, label: &str) -> Result<PathBuf> {
    let base = fs::canonicalize(base_dir)
        .with_context(|| format!("failed to canonicalize base {}", base_dir.display()))?;

    let absolute_candidate = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        base.join(candidate)
    };
    let target = if absolute_candidate.exists() {
        fs::canonicalize(&absolute_candidate)
            .with_context(|| format!("failed to canonicalize {} {}", label, candidate.display()))?
    } else {
        let parent = absolute_candidate.parent().ok_or_else(|| {
            anyhow!(
                "{} {} has no parent directory",
                label,
                absolute_candidate.display()
            )
        })?;
        let canonical_parent = fs::canonicalize(parent).with_context(|| {
            format!(
                "failed to canonicalize parent {} for {}",
                parent.display(),
                absolute_candidate.display()
            )
        })?;
        let file_name = absolute_candidate.file_name().ok_or_else(|| {
            anyhow!(
                "{} {} has no final path component",
                label,
                absolute_candidate.display()
            )
        })?;
        canonical_parent.join(file_name)
    };

    if !target.starts_with(&base) {
        bail!(
            "{} {} escapes base directory {}",
            label,
            target.display(),
            base.display()
        );
    }
    Ok(target)
}

fn temporary_path_for(path: &Path) -> PathBuf {
    let parent = path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_string();
    let pid = std::process::id();
    let rand = Uuid::new_v4().simple().to_string();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    parent.join(format!(".{file_name}.tmp.{pid}.{nanos}.{rand}"))
}

fn current_euid() -> u32 {
    unsafe { libc::geteuid() as u32 }
}

pub fn slugify(input: &str, fallback: &str) -> String {
    let mut out = String::new();
    let mut previous_dash = false;

    for c in input.chars() {
        let normalized = c.to_ascii_lowercase();
        if normalized.is_ascii_alphanumeric() {
            out.push(normalized);
            previous_dash = false;
        } else if !previous_dash {
            out.push('-');
            previous_dash = true;
        }
    }

    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        return fallback.to_string();
    }
    trimmed
}

#[cfg(test)]
#[path = "../tests/src/fsutil.rs"]
mod tests;
