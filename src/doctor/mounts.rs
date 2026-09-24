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

    match fs::read_to_string("/proc/mounts") {
        Ok(mounts) => {
            let orphaned: Vec<&str> = mounts
                .lines()
                .filter_map(|line| {
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if parts.len() >= 2 {
                        let mount_point = parts[1];
                        if Path::new(mount_point).starts_with(&sandboxes_dir) {
                            Some(mount_point)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                })
                .collect();

            if orphaned.is_empty() {
                DoctorCheck::ok(name, "no enclave-related mounts found")
            } else {
                // A running sandbox or workspace keeps several mounts below its
                // own directory: the sandbox rootfs, the workspace disk image,
                // and the home and root overlays. All of those are expected.
                let active_roots: Vec<PathBuf> = with_registry(state_dir, |reg| {
                    let mut paths = Vec::new();
                    for sandbox in reg.sandboxes.values() {
                        // Shared-base rootfs overlays stay mounted for the whole
                        // sandbox lifetime, so they are expected at any status.
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
                .unwrap_or_default();

                let truly_orphaned: Vec<&&str> = orphaned
                    .iter()
                    .filter(|mount_point| {
                        let mount_point = Path::new(mount_point);
                        !active_roots
                            .iter()
                            .any(|root| mount_point.starts_with(root))
                    })
                    .collect();

                if truly_orphaned.is_empty() {
                    DoctorCheck::ok(
                        name,
                        &format!(
                            "{} active enclave mount(s), all accounted for",
                            orphaned.len()
                        ),
                    )
                } else {
                    DoctorCheck::warn(
                        name,
                        &format!(
                            "{} orphaned mount(s) detected: {}",
                            truly_orphaned.len(),
                            truly_orphaned
                                .iter()
                                .map(|s| **s)
                                .collect::<Vec<&str>>()
                                .join(", ")
                        ),
                    )
                }
            }
        }
        Err(err) => DoctorCheck::warn(name, &format!("failed to read /proc/mounts: {err}")),
    }
}
