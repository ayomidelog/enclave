use std::fs;
use std::path::{Path, PathBuf};

use crate::registry::with_registry;

use super::DoctorCheck;

/// Report an active sandbox whose workspaces would receive no root filesystem.
///
/// A workspace session mounts its root overlay on top of the sandbox rootfs bind
/// mount, so a missing or empty rootfs silently produces a workspace that claims
/// to be running with an empty root. The registry alone cannot show this, because
/// the mount is host state the registry does not record.
pub(crate) fn check_sandbox_rootfs_mounts(state_dir: &Path) -> DoctorCheck {
    let name = "sandbox_rootfs";
    let active = match with_registry(state_dir, |reg| {
        Ok(reg
            .sandboxes
            .values()
            .filter(|sandbox| sandbox.metadata.status.rootfs_is_mounted())
            .map(|sandbox| sandbox.metadata.clone())
            .collect::<Vec<_>>())
    }) {
        Ok(active) => active,
        Err(err) => {
            return DoctorCheck::warn(name, &format!("failed to read sandbox state: {err:#}"))
        }
    };
    if active.is_empty() {
        return DoctorCheck::ok(name, "no sandbox is using a root filesystem");
    }

    let mut problems = Vec::new();
    for metadata in &active {
        let mounted = Path::new(&metadata.mounted_rootfs_path);
        if !crate::fsutil::is_mountpoint(mounted).unwrap_or(false) {
            problems.push(format!(
                "{} ({}) is not mounted",
                metadata.id,
                mounted.display()
            ));
            continue;
        }
        let empty = fs::read_dir(mounted)
            .map(|mut entries| entries.next().is_none())
            .unwrap_or(true);
        if empty {
            problems.push(format!("{} ({}) is empty", metadata.id, mounted.display()));
        }
    }
    if problems.is_empty() {
        return DoctorCheck::ok(
            name,
            &format!(
                "{} active sandbox rootfs mount(s) hold a populated root",
                active.len()
            ),
        );
    }
    DoctorCheck::warn(
        name,
        &format!(
            "{} active sandbox rootfs mount(s) cannot serve a workspace: {}; restart the sandbox",
            problems.len(),
            problems.join(", ")
        ),
    )
}

pub(crate) fn check_orphaned_mounts(state_dir: &Path) -> DoctorCheck {
    let name = "orphaned_mounts";
    let sandboxes_dir = state_dir.join("sandboxes");
    let snapshot = match crate::fsutil::MountInfoSnapshot::load() {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return DoctorCheck::warn(
                name,
                &format!("failed to read /proc/self/mountinfo: {error:#}"),
            )
        }
    };
    if snapshot.at_or_below_entries(&sandboxes_dir).is_empty() {
        return DoctorCheck::ok(name, "no enclave-related mounts found");
    }

    let active_roots = active_mount_roots(state_dir);
    let (orphaned, foreign) = classify_sandbox_mounts(&snapshot, &sandboxes_dir, &active_roots);
    if orphaned.is_empty() && foreign.is_empty() {
        return DoctorCheck::ok(
            name,
            &format!(
                "{} active enclave mount(s), all accounted for",
                snapshot.at_or_below_entries(&sandboxes_dir).len()
            ),
        );
    }

    let mut problems = Vec::new();
    if !orphaned.is_empty() {
        problems.push(format!(
            "{} orphaned mount(s) detected: {}",
            orphaned.len(),
            orphaned.join(", ")
        ));
    }
    if !foreign.is_empty() {
        problems.push(format!(
            "{} mount(s) under the state directory were not created by Enclave and are left in place: {}",
            foreign.len(),
            foreign.join(", ")
        ));
    }
    DoctorCheck::warn(name, &problems.join("; "))
}

/// The mount points a running sandbox or workspace legitimately keeps mounted.
///
/// A running sandbox or workspace keeps several mounts below its own directory:
/// the sandbox rootfs, the workspace disk image, and the home and root overlays.
/// All of those are expected.
fn active_mount_roots(state_dir: &Path) -> Vec<PathBuf> {
    with_registry(state_dir, |reg| {
        let mut paths = Vec::new();
        for sandbox in reg.sandboxes.values() {
            // Shared-base rootfs overlays stay mounted for the whole sandbox
            // lifetime, so they are expected at any status.
            paths.push(PathBuf::from(&sandbox.metadata.rootfs_path));
            if sandbox.metadata.status.rootfs_is_mounted() {
                paths.push(PathBuf::from(&sandbox.metadata.mounted_rootfs_path));
            }
            for workspace in sandbox.workspaces.values() {
                if crate::workspace::workspace_runtime_is_active(workspace) {
                    paths.push(PathBuf::from(&workspace.workspace_path));
                }
            }
        }
        Ok(paths)
    })
    .unwrap_or_default()
}

/// Split the mounts below the sandboxes tree into Enclave's own leftovers and the
/// mounts Enclave did not create.
///
/// A mount below an active root belongs to neither list. Everything else Enclave
/// created is an orphan, and everything Enclave did not create is reported
/// separately: it is still why the path cannot be cleaned up, but it is not
/// Enclave's mount to remove, so reporting it as an Enclave leftover would send
/// the operator looking for a bug that is not there.
pub(crate) fn classify_sandbox_mounts(
    snapshot: &crate::fsutil::MountInfoSnapshot,
    sandboxes_dir: &Path,
    active_roots: &[PathBuf],
) -> (Vec<String>, Vec<String>) {
    let state_root = crate::fsutil::enclave_state_root(sandboxes_dir);
    let mut orphaned = Vec::new();
    let mut foreign = Vec::new();
    for entry in snapshot.at_or_below_entries(sandboxes_dir) {
        if !state_root
            .as_deref()
            .is_some_and(|root| entry.is_enclave_owned(root))
        {
            foreign.push(entry.describe());
            continue;
        }
        if active_roots
            .iter()
            .any(|root| entry.mountpoint.starts_with(root))
        {
            continue;
        }
        orphaned.push(entry.describe());
    }
    (orphaned, foreign)
}
