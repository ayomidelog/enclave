use std::fs::{self};
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use uuid::Uuid;

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

pub(crate) fn temporary_path_for(path: &Path) -> PathBuf {
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

pub(crate) fn current_euid() -> u32 {
    unsafe { libc::geteuid() as u32 }
}
