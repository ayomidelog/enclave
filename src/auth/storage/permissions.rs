//! The checks that make reading a token file safe.
//!
//! A token file is a secret at rest, so it is only ever read after its ownership
//! and mode have been checked. The checks live here rather than at each call site
//! because every path that reads a token has to pass them, and a caller that
//! forgot would read a file another user could have replaced.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

use anyhow::{bail, Context, Result};

/// Whether a token file is one this process may read.
///
/// A regular file, not a symlink, owned by the effective uid, mode 0600. A
/// symlink is refused rather than followed because following one would read a
/// file outside the namespace the caller asked for.
pub(in crate::auth) fn validate_token_permissions(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to stat token file {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        bail!("refusing to use symlink token file {}", path.display());
    }
    if !metadata.is_file() {
        bail!("token path {} is not a regular file", path.display());
    }
    let expected_uid = unsafe { libc::geteuid() as u32 };
    if metadata.uid() != expected_uid {
        bail!(
            "token file {} must be owned by uid {}, found uid {}",
            path.display(),
            expected_uid,
            metadata.uid()
        );
    }
    let mode = metadata.permissions().mode() & 0o777;
    if mode != 0o600 {
        bail!(
            "token file {} must have mode 0600, found {:o}",
            path.display(),
            mode
        );
    }
    Ok(())
}

/// Create a directory that only its owner can enter, and keep it that way.
///
/// `ensure_secure_dir` only tightens a directory that is group- or
/// world-*writable*, which leaves a directory created under the usual umask at
/// 0755. A token directory has to be 0700 whatever the umask is, so the mode is
/// set here rather than inferred.
pub(super) fn ensure_private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))?;
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("failed to stat {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        bail!("{} must not be a symlink", path.display());
    }
    if !metadata.is_dir() {
        bail!("{} is not a directory", path.display());
    }
    let expected_uid = unsafe { libc::geteuid() as u32 };
    if metadata.uid() != expected_uid {
        bail!(
            "{} must be owned by uid {}, found uid {}",
            path.display(),
            expected_uid,
            metadata.uid()
        );
    }
    if metadata.permissions().mode() & 0o777 != 0o700 {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("failed to set mode 0700 on {}", path.display()))?;
    }
    Ok(())
}

/// Refuse a directory a token is about to be read from unless it is private.
pub(super) fn validate_private_dir(path: &Path) -> Result<()> {
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("failed to stat {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        bail!("{} must not be a symlink", path.display());
    }
    if !metadata.is_dir() {
        bail!("{} is not a directory", path.display());
    }
    let expected_uid = unsafe { libc::geteuid() as u32 };
    if metadata.uid() != expected_uid {
        bail!(
            "{} must be owned by uid {}, found uid {}",
            path.display(),
            expected_uid,
            metadata.uid()
        );
    }
    let mode = metadata.permissions().mode() & 0o777;
    if mode != 0o700 {
        bail!("{} must have mode 0700, found {:o}", path.display(), mode);
    }
    Ok(())
}
