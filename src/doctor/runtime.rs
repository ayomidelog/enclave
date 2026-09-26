use std::fs;
use std::path::Path;

use crate::registry::with_registry;

use super::DoctorCheck;

pub(crate) fn check_stale_runtime_state(state_dir: &Path) -> DoctorCheck {
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
pub(crate) fn check_workspace_tmp_integrity(state_dir: &Path) -> DoctorCheck {
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
pub(crate) fn check_workspace_storage(state_dir: &Path) -> DoctorCheck {
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
