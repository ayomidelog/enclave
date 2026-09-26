use std::fs::{self, File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use anyhow::{Context, Result};

pub fn reflink_copy_file(source: &Path, destination: &Path) -> Result<bool> {
    let source_file = File::open(source)
        .with_context(|| format!("failed to open reflink source {}", source.display()))?;
    let destination_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(destination)
        .with_context(|| {
            format!(
                "failed to create reflink destination {}",
                destination.display()
            )
        })?;
    let result = unsafe {
        libc::ioctl(
            destination_file.as_raw_fd(),
            libc::FICLONE as libc::c_ulong,
            source_file.as_raw_fd(),
        )
    };
    if result == 0 {
        let mode = source_file
            .metadata()
            .with_context(|| format!("failed to stat {}", source.display()))?
            .permissions();
        fs::set_permissions(destination, mode)
            .with_context(|| format!("failed to preserve mode on {}", destination.display()))?;
        return Ok(true);
    }

    let error = std::io::Error::last_os_error();
    drop(destination_file);
    let _ = fs::remove_file(destination);
    if matches!(
        error.raw_os_error(),
        Some(libc::EOPNOTSUPP | libc::EXDEV | libc::EINVAL | libc::ENOTTY)
    ) {
        return Ok(false);
    }
    Err(error).with_context(|| {
        format!(
            "failed to clone {} to {} with FICLONE",
            source.display(),
            destination.display()
        )
    })
}

pub fn copy_file_range_file(source: &Path, destination: &Path) -> Result<bool> {
    let source_file = File::open(source)
        .with_context(|| format!("failed to open copy source {}", source.display()))?;
    let source_metadata = source_file
        .metadata()
        .with_context(|| format!("failed to stat copy source {}", source.display()))?;
    let destination_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(destination)
        .with_context(|| {
            format!(
                "failed to create copy destination {}",
                destination.display()
            )
        })?;
    let mut copied = 0u64;
    while copied < source_metadata.len() {
        let amount = unsafe {
            libc::copy_file_range(
                source_file.as_raw_fd(),
                std::ptr::null_mut(),
                destination_file.as_raw_fd(),
                std::ptr::null_mut(),
                (source_metadata.len() - copied).min(4 * 1024 * 1024) as usize,
                0,
            )
        };
        if amount > 0 {
            copied = copied.saturating_add(amount as u64);
            continue;
        }
        if amount == 0 {
            break;
        }
        let error = std::io::Error::last_os_error();
        drop(destination_file);
        let _ = fs::remove_file(destination);
        if matches!(
            error.raw_os_error(),
            Some(libc::EOPNOTSUPP | libc::EXDEV | libc::EINVAL | libc::ENOSYS)
        ) {
            return Ok(false);
        }
        return Err(error).with_context(|| {
            format!(
                "failed to copy {} to {} with copy_file_range",
                source.display(),
                destination.display()
            )
        });
    }
    if copied != source_metadata.len() {
        drop(destination_file);
        let _ = fs::remove_file(destination);
        return Ok(false);
    }
    fs::set_permissions(destination, source_metadata.permissions())
        .with_context(|| format!("failed to preserve mode on {}", destination.display()))?;
    Ok(true)
}
