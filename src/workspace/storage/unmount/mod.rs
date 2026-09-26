//! Detaching the storage mounts a workspace owns.
//!
//! A workspace owns its mounts and its disk image's loop device, and nothing
//! else, so teardown has to answer three questions: which mounts are Enclave's,
//! how to detach one, and whether the loop device was released. Each is a
//! module: verify decides ownership and proves the result, detach makes the
//! unmount call, and loopback handles the disk image.

mod detach;
mod loopback;
mod verify;

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

use crate::workspace::types::WorkspaceMetadata;

// Used by the entry points below.
use detach::unmount_workspace_path;

use verify::{describe_mount, is_enclave_owned, verify_no_mounts_below};

// The rest of the storage module reaches these through `super::`.
pub(crate) use loopback::{
    loop_device_is_mounted, loop_devices_for_image, namespaces_mounting_devices,
    verify_disk_image_loop_detached,
};
pub(crate) use verify::{remaining_mount_detail, workspace_owner_is_dead};

// The storage tests reach these through `super::`; nothing in the daemon does.
#[cfg(test)]
pub(crate) use crate::fsutil::parse_losetup_for_image;
#[cfg(test)]
pub(crate) use detach::mount_holders;
#[cfg(test)]
pub(crate) use detach::unmount_error;
#[cfg(test)]
pub(crate) use verify::{mounts_below, parse_mountinfo_mountpoints};

pub fn ensure_workspace_storage_unmounted(workspace: &WorkspaceMetadata) -> Result<()> {
    let workspace_root = Path::new(&workspace.workspace_path);
    let snapshot = crate::fsutil::MountInfoSnapshot::load()?;
    let owner_is_dead = workspace_owner_is_dead(workspace);
    let state_dir = crate::fsutil::enclave_state_root(workspace_root);
    for entry in snapshot.at_or_below_entries(workspace_root) {
        if !is_enclave_owned(entry, state_dir.as_deref()) {
            continue;
        }
        unmount_workspace_path(&entry.mountpoint, owner_is_dead)?;
    }
    verify_no_mounts_below(workspace_root)?;
    verify_disk_image_loop_detached(workspace)?;
    Ok(())
}

/// Unmount all workspace storage using one mountinfo snapshot. This avoids a
/// full `/proc/self/mountinfo` scan for every workspace during sandbox stop.
pub(crate) fn ensure_workspace_storage_unmounted_many(
    workspaces: &[WorkspaceMetadata],
) -> Result<()> {
    let snapshot = crate::fsutil::MountInfoSnapshot::load()?;
    let mut mountpoints = Vec::new();
    for workspace in workspaces {
        let root = Path::new(&workspace.workspace_path);
        let state_dir = crate::fsutil::enclave_state_root(root);
        let owner_is_dead = workspace_owner_is_dead(workspace);
        for entry in snapshot.at_or_below_entries(root) {
            if !is_enclave_owned(entry, state_dir.as_deref()) {
                continue;
            }
            mountpoints.push((entry.mountpoint.clone(), owner_is_dead));
        }
    }
    mountpoints.sort_by(|left, right| {
        right
            .0
            .components()
            .count()
            .cmp(&left.0.components().count())
    });
    mountpoints.dedup_by(|left, right| left.0 == right.0);
    for (path, owner_is_dead) in mountpoints {
        unmount_workspace_path(&path, owner_is_dead)?;
    }
    let snapshot = crate::fsutil::MountInfoSnapshot::load()?;
    for workspace in workspaces {
        let root = Path::new(&workspace.workspace_path);
        let owned = snapshot.owned_at_or_below(root);
        let foreign = snapshot.foreign_at_or_below(root);
        if !foreign.is_empty() {
            tracing::warn!(
                "workspace '{}': {} mount(s) below {} were not created by Enclave and were left in place: {}",
                workspace.id,
                foreign.len(),
                workspace.workspace_path,
                foreign.join("; ")
            );
        }
        if !owned.is_empty() {
            bail!(
                "workspace '{}' still has {} Enclave mount(s) below {} after unmount: {}",
                workspace.id,
                owned.len(),
                workspace.workspace_path,
                remaining_mount_detail(&owned, &foreign)
            );
        }
    }
    for workspace in workspaces {
        verify_disk_image_loop_detached(workspace)?;
    }
    Ok(())
}

/// What a sweep of the sandboxes tree for mounts a previous run left behind found.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct StaleMountSweep {
    /// Enclave's own mounts that were unmounted.
    pub unmounted: usize,
    /// Mounts under the tree that Enclave did not create, described for a report.
    pub foreign: Vec<String>,
}

pub(crate) fn unmount_mounts_at_or_below_excluding(
    root: &Path,
    excluded_roots: &[PathBuf],
) -> Result<StaleMountSweep> {
    let snapshot = crate::fsutil::MountInfoSnapshot::load()?;
    let state_dir = crate::fsutil::enclave_state_root(root);
    let mut sweep = StaleMountSweep::default();
    for entry in snapshot.at_or_below_entries(root) {
        if excluded_roots
            .iter()
            .any(|excluded| entry.mountpoint.starts_with(excluded))
        {
            continue;
        }
        if !is_enclave_owned(entry, state_dir.as_deref()) {
            sweep.foreign.push(describe_mount(entry));
            continue;
        }
        unmount_workspace_path(&entry.mountpoint, true)?;
        sweep.unmounted += 1;
    }
    Ok(sweep)
}
