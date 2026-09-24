use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::operation::OperationStatus;
use crate::registry::{ensure_registry, repair_registry, with_registry};
use crate::sandbox::cgroup;
use crate::workspace::WorkspaceStatus;

mod cgroups;
mod network;

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
    fn ok(name: &str, detail: &str) -> Self {
        Self {
            name: name.to_string(),
            status: "ok".to_string(),
            detail: detail.to_string(),
        }
    }

    fn warn(name: &str, detail: &str) -> Self {
        Self {
            name: name.to_string(),
            status: "warn".to_string(),
            detail: detail.to_string(),
        }
    }
}

pub fn run_doctor(state_dir: &Path) -> Result<DoctorReport> {
    let checks = vec![
        check_registry_consistency(state_dir),
        check_orphaned_mounts(state_dir),
        check_stale_cgroups(state_dir),
        network::check_workspace_network(state_dir),
        network::check_workspace_loop_devices(state_dir),
        check_stale_runtime_state(state_dir),
        check_operation_journal(state_dir),
        check_cgroup_v2_availability(),
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
            if matches!(
                sandbox.metadata.status,
                crate::sandbox::SandboxStatus::Running | crate::sandbox::SandboxStatus::Paused
            ) {
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

fn check_registry_consistency(state_dir: &Path) -> DoctorCheck {
    let name = "registry_consistency";
    let registry_path = state_dir.join("registry.json");
    if !registry_path.exists() {
        return DoctorCheck::warn(name, "registry.json not found");
    }

    match with_registry(state_dir, |registry| {
        let mut issues = Vec::new();
        let sandboxes_dir = state_dir.join("sandboxes");

        for (sandbox_id, sandbox) in &registry.sandboxes {
            let sandbox_path = std::path::PathBuf::from(&sandbox.metadata.sandbox_path);
            if !sandbox_path.exists() {
                issues.push(format!(
                    "sandbox '{}' registered but directory missing",
                    sandbox_id
                ));
            }
            for (workspace_id, workspace) in &sandbox.workspaces {
                let ws_path = std::path::PathBuf::from(&workspace.workspace_path);
                if !ws_path.exists() {
                    issues.push(format!(
                        "workspace '{}' in sandbox '{}' registered but directory missing",
                        workspace_id, sandbox_id
                    ));
                }
            }
        }

        if sandboxes_dir.exists() {
            if let Ok(entries) = fs::read_dir(&sandboxes_dir) {
                for entry in entries.flatten() {
                    let dir_name = entry.file_name().to_string_lossy().to_string();
                    if dir_name == "rootfs-cache" {
                        continue;
                    }
                    if entry.path().is_dir()
                        && !registry.sandboxes.contains_key(&dir_name)
                        && entry.path().join("sandbox.json").exists()
                    {
                        issues.push(format!(
                            "sandbox directory '{}' exists on disk but not in registry",
                            dir_name
                        ));
                    }
                }
            }
        }

        Ok(issues)
    }) {
        Ok(issues) => {
            if issues.is_empty() {
                DoctorCheck::ok(name, "registry is consistent with disk state")
            } else {
                DoctorCheck::warn(
                    name,
                    &format!("{} issue(s) found: {}", issues.len(), issues.join("; ")),
                )
            }
        }
        Err(err) => DoctorCheck::warn(name, &format!("failed to read registry: {err:#}")),
    }
}

fn check_orphaned_mounts(state_dir: &Path) -> DoctorCheck {
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
                let running_mounted: Vec<String> = with_registry(state_dir, |reg| {
                    let mut paths = Vec::new();
                    for sandbox in reg.sandboxes.values() {
                        if matches!(
                            sandbox.metadata.status,
                            crate::sandbox::SandboxStatus::Running
                                | crate::sandbox::SandboxStatus::Paused
                        ) {
                            paths.push(sandbox.metadata.mounted_rootfs_path.clone());
                        }
                    }
                    Ok(paths)
                })
                .unwrap_or_default();

                let truly_orphaned: Vec<&&str> = orphaned
                    .iter()
                    .filter(|mp| !running_mounted.contains(&mp.to_string()))
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

fn check_stale_cgroups(state_dir: &Path) -> DoctorCheck {
    cgroups::check_stale_workspace_cgroups(state_dir)
}

fn check_stale_runtime_state(state_dir: &Path) -> DoctorCheck {
    let name = "stale_runtime_state";

    match with_registry(state_dir, |registry| {
        let mut stale_count = 0usize;
        for sandbox in registry.sandboxes.values() {
            for workspace in sandbox.workspaces.values() {
                if workspace.status != WorkspaceStatus::Running {
                    continue;
                }
                let pid_alive = workspace
                    .runtime_pid
                    .map(|pid| {
                        crate::workspace::session_process_matches(
                            pid,
                            workspace.runtime_starttime_ticks,
                        )
                    })
                    .unwrap_or(false);
                if !pid_alive {
                    stale_count += 1;
                }
            }
        }
        Ok(stale_count)
    }) {
        Ok(0) => DoctorCheck::ok(name, "all running workspaces have active session processes"),
        Ok(count) => DoctorCheck::warn(
            name,
            &format!(
                "{} workspace(s) marked running but session process is gone; \
                 run 'enclave registry repair' to reconcile",
                count
            ),
        ),
        Err(err) => DoctorCheck::warn(name, &format!("failed to check: {err:#}")),
    }
}

fn check_operation_journal(state_dir: &Path) -> DoctorCheck {
    let name = "operation_journal";
    let root = state_dir.join("operations");
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return DoctorCheck::ok(name, "no operation journal exists");
        }
        Err(error) => {
            return DoctorCheck::warn(name, &format!("failed to read {}: {error}", root.display()))
        }
    };
    let mut unfinished = Vec::new();
    let mut malformed = 0usize;
    for entry in entries.flatten() {
        if entry
            .path()
            .extension()
            .and_then(|extension| extension.to_str())
            != Some("json")
        {
            continue;
        }
        match fs::read(entry.path())
            .ok()
            .and_then(|raw| serde_json::from_slice::<crate::operation::OperationRecord>(&raw).ok())
        {
            Some(record)
                if matches!(
                    record.status,
                    OperationStatus::Planned | OperationStatus::Running
                ) =>
            {
                unfinished.push(format!("{} {} ({})", record.id, record.kind, record.target));
            }
            Some(_) => {}
            None => malformed += 1,
        }
    }
    if malformed > 0 || !unfinished.is_empty() {
        let mut details = Vec::new();
        if !unfinished.is_empty() {
            details.push(format!("unfinished: {}", unfinished.join("; ")));
        }
        if malformed > 0 {
            details.push(format!("{malformed} malformed journal file(s)"));
        }
        DoctorCheck::warn(name, &details.join("; "))
    } else {
        DoctorCheck::ok(name, "all operation journal records are terminal")
    }
}

fn check_cgroup_v2_availability() -> DoctorCheck {
    let name = "cgroup_v2";
    if cgroup::is_cgroup_v2_available() {
        let controllers = cgroup::available_controllers();
        DoctorCheck::ok(
            name,
            &format!(
                "cgroup v2 available; controllers: {}",
                if controllers.is_empty() {
                    "none".to_string()
                } else {
                    controllers.join(", ")
                }
            ),
        )
    } else {
        DoctorCheck::warn(
            name,
            "cgroup v2 not available; resource limits will use rlimit only",
        )
    }
}

#[cfg(test)]
#[path = "../tests/src/doctor.rs"]
mod tests;
