use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::operation::OperationStatus;
use crate::registry::{ensure_registry, repair_registry, with_registry};
use crate::sandbox::cgroup;

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
        check_workspace_tmp_integrity(state_dir),
        check_workspace_storage(state_dir),
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

fn check_stale_cgroups(state_dir: &Path) -> DoctorCheck {
    cgroups::check_stale_workspace_cgroups(state_dir)
}

fn check_stale_runtime_state(state_dir: &Path) -> DoctorCheck {
    let name = "stale_runtime_state";

    let (stale_count, transitional) = match with_registry(state_dir, |registry| {
        let mut stale_count = 0usize;
        let mut transitional = Vec::new();
        for sandbox in registry.sandboxes.values() {
            for workspace in sandbox.workspaces.values() {
                if workspace.status.is_transitional() {
                    transitional.push(workspace.id.clone());
                    continue;
                }
                if !workspace.status.is_running() {
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
        Ok((stale_count, transitional))
    }) {
        Ok(value) => value,
        Err(err) => return DoctorCheck::warn(name, &format!("failed to check: {err:#}")),
    };

    let mut findings = Vec::new();
    if stale_count > 0 {
        findings.push(format!(
            "{stale_count} workspace(s) marked running but session process is gone"
        ));
    }
    if !transitional.is_empty() {
        findings.push(format!(
            "{} workspace(s) have an unfinished lifecycle transition ({})",
            transitional.len(),
            transitional.join(", ")
        ));
    }
    if findings.is_empty() {
        return DoctorCheck::ok(name, "all running workspaces have active session processes");
    }
    findings.push("run 'enclave registry repair' to reconcile".to_string());
    DoctorCheck::warn(name, &findings.join("; "))
}

/// Check that every running workspace has a usable `/tmp`.
///
/// The workspace `/tmp` is a bind mount of a directory on the workspace's own
/// filesystem. If that directory is replaced while the runtime is alive, the
/// mount keeps pointing at the old, unlinked inode and every write under `/tmp`
/// fails with `ENOENT` for the rest of the runtime's life. That is invisible to
/// the registry and to the mount inventory, so it needs its own check.
fn check_workspace_tmp_integrity(state_dir: &Path) -> DoctorCheck {
    use std::os::unix::fs::MetadataExt;

    let name = "workspace_tmp";
    let workspaces = match crate::workspace::list_workspaces(state_dir, None) {
        Ok(workspaces) => workspaces,
        Err(err) => return DoctorCheck::warn(name, &format!("failed to check: {err:#}")),
    };

    let mut broken = Vec::new();
    let mut checked = 0usize;
    for workspace in workspaces {
        if !crate::workspace::workspace_runtime_is_active(&workspace) {
            continue;
        }
        let Some(pid) = workspace.runtime_pid else {
            continue;
        };
        checked += 1;
        let tmp = Path::new("/proc").join(pid.to_string()).join("root/tmp");
        let metadata = match fs::metadata(&tmp) {
            Ok(metadata) => metadata,
            Err(error) => {
                broken.push(format!("{}: /tmp is unreadable ({error})", workspace.id));
                continue;
            }
        };
        if !metadata.is_dir() {
            broken.push(format!("{}: /tmp is not a directory", workspace.id));
            continue;
        }
        if metadata.nlink() < 2 {
            broken.push(format!(
                "{}: /tmp references an unlinked directory, so every /tmp write fails; \
                 restart the workspace to repair",
                workspace.id
            ));
            continue;
        }
        if metadata.mode() & 0o1777 != 0o1777 {
            broken.push(format!(
                "{}: /tmp mode is {:o} instead of 1777",
                workspace.id,
                metadata.mode() & 0o7777
            ));
            continue;
        }
        if crate::workspace::workspace_uses_disk_image(&workspace) {
            let backing = crate::workspace::workspace_tmp_path(&workspace);
            match fs::metadata(&backing) {
                Ok(backing_metadata) if backing_metadata.dev() == metadata.dev() => {}
                Ok(_) => broken.push(format!(
                    "{}: /tmp is not backed by the workspace filesystem",
                    workspace.id
                )),
                Err(error) => broken.push(format!(
                    "{}: /tmp backing directory {} is unavailable ({error})",
                    workspace.id,
                    backing.display()
                )),
            }
        }
    }

    if broken.is_empty() {
        return DoctorCheck::ok(
            name,
            &format!("{checked} running workspace(s) have a usable /tmp"),
        );
    }
    DoctorCheck::warn(name, &broken.join("; "))
}

/// Check that every workspace's storage source is actually usable.
///
/// A workspace whose quota-backed image is not mounted, or whose host source
/// directory disappeared, cannot start: the session resolves the source inside
/// its own mount namespace and fails with a path error that does not name the
/// host path. Reporting it here turns that into an actionable finding.
fn check_workspace_storage(state_dir: &Path) -> DoctorCheck {
    let name = "workspace_storage";
    let workspaces = match crate::workspace::list_workspaces(state_dir, None) {
        Ok(workspaces) => workspaces,
        Err(err) => return DoctorCheck::warn(name, &format!("failed to check: {err:#}")),
    };

    let mut broken = Vec::new();
    for workspace in workspaces {
        // A stopped workspace is expected to have its storage unmounted; only an
        // active one has to be ready to serve commands.
        if !crate::workspace::workspace_runtime_is_active(&workspace) {
            continue;
        }
        let source = workspace
            .home_mount_source_path
            .as_deref()
            .unwrap_or(&workspace.filesystem_path);
        if crate::workspace::workspace_uses_disk_image(&workspace) {
            if !crate::fsutil::is_mountpoint(Path::new(source)).unwrap_or(false) {
                broken.push(format!(
                    "{}: quota-backed storage {} is not mounted",
                    workspace.id, source
                ));
            }
            continue;
        }
        match fs::symlink_metadata(source) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => broken.push(format!(
                "{}: source path {} is not a directory",
                workspace.id, source
            )),
            Err(error) => broken.push(format!(
                "{}: source path {} is unavailable ({error})",
                workspace.id, source
            )),
        }
    }

    if broken.is_empty() {
        return DoctorCheck::ok(name, "all workspace storage sources are usable");
    }
    DoctorCheck::warn(name, &broken.join("; "))
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
