use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::registry::{ensure_registry, repair_registry, with_registry};

mod capabilities;
mod cgroups;
mod journal;
mod mounts;
mod network;
mod registry;
mod runtime;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DoctorReport {
    pub status: String,
    pub checks: Vec<DoctorCheck>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorCheck {
    pub name: String,
    pub status: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DoctorRepairReport {
    pub registry: crate::registry::RepairReport,
    pub unmounted_stale_mounts: usize,
    pub reconciled_workspace_mounts: usize,
    #[serde(default)]
    pub removed_stale_workspace_cgroups: usize,
    pub daemon_state_consistent: bool,
}

impl DoctorCheck {
    pub(super) fn ok(name: &str, detail: &str) -> Self {
        Self {
            name: name.to_string(),
            status: "ok".to_string(),
            detail: detail.to_string(),
        }
    }

    pub(super) fn warn(name: &str, detail: &str) -> Self {
        Self {
            name: name.to_string(),
            status: "warn".to_string(),
            detail: detail.to_string(),
        }
    }
}
pub fn run_doctor(state_dir: &Path) -> Result<DoctorReport> {
    let checks = vec![
        registry::check_registry_consistency(state_dir),
        mounts::check_orphaned_mounts(state_dir),
        cgroups::check_stale_workspace_cgroups(state_dir),
        network::check_workspace_network(state_dir),
        network::check_workspace_loop_devices(state_dir),
        runtime::check_stale_runtime_state(state_dir),
        runtime::check_workspace_tmp_integrity(state_dir),
        runtime::check_workspace_storage(state_dir),
        journal::check_operation_journal(state_dir),
        capabilities::check_cgroup_v2_availability(),
    ];

    let all_ok = checks.iter().all(|c| c.status == "ok");
    let status = if all_ok {
        "healthy".to_string()
    } else {
        "issues_detected".to_string()
    };

    Ok(DoctorReport { status, checks })
}

pub fn repair_doctor(state_dir: &Path, socket_path: &Path) -> Result<DoctorRepairReport> {
    fs::create_dir_all(state_dir.join("sandboxes"))
        .with_context(|| format!("failed to initialize storage at {}", state_dir.display()))?;
    ensure_registry(state_dir)?;

    let (active_roots, stopped_workspaces) = with_registry(state_dir, |registry| {
        let mut active_roots = Vec::new();
        let mut stopped_workspaces = Vec::new();
        for sandbox in registry.sandboxes.values() {
            // A shared-base rootfs overlay is mounted for the sandbox's whole
            // lifetime, running or not, so it is never a stale mount.
            active_roots.push(PathBuf::from(&sandbox.metadata.rootfs_path));
            if sandbox.metadata.status.rootfs_is_mounted() {
                active_roots.push(PathBuf::from(&sandbox.metadata.mounted_rootfs_path));
            }
            for workspace in sandbox.workspaces.values() {
                if crate::workspace::workspace_runtime_is_active(workspace) {
                    active_roots.push(PathBuf::from(&workspace.workspace_path));
                } else {
                    stopped_workspaces.push(workspace.clone());
                }
            }
        }
        Ok((active_roots, stopped_workspaces))
    })?;

    let mut reconciled_workspace_mounts = 0usize;
    for workspace in stopped_workspaces {
        crate::workspace::ensure_workspace_storage_unmounted(&workspace)?;
        reconciled_workspace_mounts += 1;
    }
    let unmounted_stale_mounts = crate::workspace::unmount_mounts_at_or_below_excluding(
        &state_dir.join("sandboxes"),
        &active_roots,
    )?;
    let ownership = cgroups::cgroup_ownership(state_dir)?;
    let removed_stale_workspace_cgroups =
        cgroups::remove_empty_workspace_cgroups(Path::new(cgroups::CGROUP_ROOT), &ownership)?;
    let registry = repair_registry(state_dir, false)?;

    let daemon_state_consistent = crate::daemon::state_lock::read_state_lock_record(state_dir)?
        .is_some_and(|record| {
            record.pid == std::process::id()
                && record.socket == socket_path.to_string_lossy()
                && record.binary_version == env!("CARGO_PKG_VERSION")
        });
    if !daemon_state_consistent {
        anyhow::bail!(
            "daemon state lock does not match pid={} socket={} version={}",
            std::process::id(),
            socket_path.display(),
            env!("CARGO_PKG_VERSION")
        );
    }

    Ok(DoctorRepairReport {
        registry,
        unmounted_stale_mounts,
        reconciled_workspace_mounts,
        removed_stale_workspace_cgroups,
        daemon_state_consistent,
    })
}
#[cfg(test)]
#[path = "../../tests/src/doctor.rs"]
mod tests;

// The checks live in their own modules; the doctor tests reach them through the
// module root so the test file stays one flat list of check behaviours.
#[cfg(test)]
pub(crate) use capabilities::check_cgroup_v2_availability;
#[cfg(test)]
pub(crate) use cgroups::check_stale_workspace_cgroups as check_stale_cgroups;
#[cfg(test)]
pub(crate) use journal::check_operation_journal;
#[cfg(test)]
pub(crate) use mounts::check_orphaned_mounts;
