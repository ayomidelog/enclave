use std::fs;
use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::registry::with_registry;
use crate::sandbox::cgroup;
use crate::workspace::WorkspaceStatus;

const CGROUP_ROOT: &str = "/sys/fs/cgroup";
const ENCLAVE_CGROUP_PREFIX: &str = "enclave-ws-";

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
        check_stale_cgroups(),
        check_stale_runtime_state(state_dir),
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
                        if sandbox.metadata.status == crate::sandbox::SandboxStatus::Running {
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

fn check_stale_cgroups() -> DoctorCheck {
    let name = "stale_cgroups";

    if !cgroup::is_cgroup_v2_available() {
        return DoctorCheck::ok(name, "cgroup v2 not available; skipped");
    }

    let cgroup_root = Path::new(CGROUP_ROOT);
    let entries = match fs::read_dir(cgroup_root) {
        Ok(entries) => entries,
        Err(err) => {
            return DoctorCheck::warn(
                name,
                &format!("failed to read {}: {err}", cgroup_root.display()),
            );
        }
    };

    let mut stale = Vec::new();
    for entry in entries.flatten() {
        let dir_name = entry.file_name().to_string_lossy().to_string();
        if !dir_name.starts_with(ENCLAVE_CGROUP_PREFIX) {
            continue;
        }
        if !entry.path().is_dir() {
            continue;
        }
        let procs_path = entry.path().join("cgroup.procs");
        let has_procs = fs::read_to_string(&procs_path)
            .map(|content| !content.trim().is_empty())
            .unwrap_or(false);
        if !has_procs {
            stale.push(dir_name);
        }
    }

    if stale.is_empty() {
        DoctorCheck::ok(name, "no stale enclave cgroups found")
    } else {
        DoctorCheck::warn(
            name,
            &format!(
                "{} stale cgroup(s) found: {}",
                stale.len(),
                stale.join(", ")
            ),
        )
    }
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
