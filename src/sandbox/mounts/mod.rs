//! The sandbox rootfs mounts: the shared-base overlay, the bind mount a running
//! sandbox hands its workspaces, and detaching both.

mod overlay;
mod paths;
mod unmount;

use std::fs;
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};

use super::types::SandboxMetadata;

use paths::validate_mount_paths;

pub use overlay::{ensure_rootfs_overlay_mounted, unmount_rootfs_overlay};
pub use unmount::ensure_rootfs_unmounted;

// The mount tests reach these directly, because each is pure and each is what a
// mount call depends on for correctness.
#[cfg(test)]
pub(crate) use overlay::{overlay_option_path, rootfs_overlay_paths};
#[cfg(test)]
pub(crate) use unmount::is_already_unmounted_errno;

pub fn ensure_rootfs_mounted(metadata: &SandboxMetadata) -> Result<()> {
    ensure_rootfs_overlay_mounted(metadata)?;
    let (rootfs_path, mounted_rootfs_path) = validate_mount_paths(metadata)?;
    if crate::fsutil::is_mountpoint(Path::new(&mounted_rootfs_path))? {
        return Ok(());
    }

    crate::fsutil::bind_mount(Path::new(&rootfs_path), Path::new(&mounted_rootfs_path))
        .map_err(|error| anyhow!("failed to mount sandbox rootfs: {error}"))?;
    crate::fsutil::make_mount_private(Path::new(&mounted_rootfs_path))
        .map_err(|error| anyhow!("failed to mark mount private: {error}"))?;

    Ok(())
}

/// Make sure a running sandbox hands its workspaces a usable rootfs.
///
/// The rootfs a workspace session receives is the bind mount the daemon owns at
/// `mounted_rootfs_path`, not the rootfs directory itself. That mount is host
/// mount state, so a stop/start cycle or any interrupted teardown can leave the
/// registry saying `running` while the bind is gone. A workspace started in that
/// state would see an empty root and still be recorded as running, which is a
/// silent failure rather than a reported one.
///
/// Re-establish the mount, which is idempotent, and then prove the result is a
/// mount point holding a populated rootfs.
pub fn ensure_rootfs_ready_for_workspace(metadata: &SandboxMetadata) -> Result<()> {
    ensure_rootfs_mounted(metadata)?;
    let mounted_rootfs_path = Path::new(&metadata.mounted_rootfs_path);
    if !crate::fsutil::is_mountpoint(mounted_rootfs_path)? {
        bail!(
            "sandbox '{}' rootfs {} is not mounted; the sandbox cannot hand a root filesystem to a workspace",
            metadata.id,
            mounted_rootfs_path.display()
        );
    }
    let mut entries = fs::read_dir(mounted_rootfs_path).with_context(|| {
        format!(
            "failed to read sandbox '{}' rootfs {}",
            metadata.id,
            mounted_rootfs_path.display()
        )
    })?;
    if entries.next().is_none() {
        bail!(
            "sandbox '{}' rootfs {} is empty; the sandbox was not bootstrapped or its rootfs was replaced",
            metadata.id,
            mounted_rootfs_path.display()
        );
    }
    Ok(())
}

#[cfg(test)]
#[path = "../../../tests/src/sandbox/mounts.rs"]
mod tests;
