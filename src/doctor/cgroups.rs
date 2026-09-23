use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::sandbox::cgroup;

use super::DoctorCheck;

pub(super) const CGROUP_ROOT: &str = "/sys/fs/cgroup";
const SANDBOX_CGROUP_PREFIX: &str = "enclave-sb-";
const WORKSPACE_CGROUP_PREFIX: &str = "enclave-ws-";

#[derive(Default)]
pub(super) struct CgroupOwnership {
    sandbox_paths: HashSet<PathBuf>,
    workspace_paths: HashMap<PathBuf, String>,
}

pub(super) fn check_stale_workspace_cgroups(state_dir: &Path) -> DoctorCheck {
    const NAME: &str = "stale_cgroups";

    if !cgroup::is_cgroup_v2_available() {
        return DoctorCheck::ok(NAME, "cgroup v2 not available; skipped");
    }

    let ownership = match cgroup_ownership(state_dir) {
        Ok(ownership) => ownership,
        Err(err) => {
            return DoctorCheck::warn(
                NAME,
                &format!("failed to inspect registry ownership: {err:#}"),
            )
        }
    };

    match empty_workspace_cgroups(Path::new(CGROUP_ROOT)) {
        Ok(stale) if stale.is_empty() => {
            DoctorCheck::ok(NAME, "no empty enclave workspace cgroups found")
        }
        Ok(stale) => {
            let details = stale
                .iter()
                .map(|path| {
                    ownership
                        .workspace_paths
                        .get(path)
                        .map(|owner| format!("{owner} ({}) has no processes", path.display()))
                        .unwrap_or_else(|| {
                            format!("untracked ({}) has no processes", path.display())
                        })
                })
                .collect::<Vec<_>>()
                .join("; ");
            DoctorCheck::warn(
                NAME,
                &format!("{} empty workspace cgroup(s) found: {details}", stale.len()),
            )
        }
        Err(err) => DoctorCheck::warn(NAME, &format!("failed to inspect cgroups: {err}")),
    }
}

pub(super) fn cgroup_ownership(state_dir: &Path) -> anyhow::Result<CgroupOwnership> {
    crate::registry::with_registry(state_dir, |registry| {
        let mut ownership = CgroupOwnership::default();
        for (registry_id, sandbox) in &registry.sandboxes {
            let sandbox_id = &sandbox.metadata.id;
            if registry_id != sandbox_id || !is_safe_id(sandbox_id) {
                continue;
            }
            let sandbox_path =
                Path::new(CGROUP_ROOT).join(format!("{SANDBOX_CGROUP_PREFIX}{sandbox_id}"));
            ownership.sandbox_paths.insert(sandbox_path.clone());
            for workspace in sandbox.workspaces.values() {
                if let Some(pid) = workspace.runtime_pid {
                    ownership.workspace_paths.insert(
                        sandbox_path.join(format!(
                            "{WORKSPACE_CGROUP_PREFIX}{sandbox_id}-{}",
                            workspace.id
                        )),
                        format!(
                            "sandbox {sandbox_id}, workspace {} (pid {pid})",
                            workspace.id
                        ),
                    );
                    ownership.workspace_paths.insert(
                        sandbox_path.join(format!("{WORKSPACE_CGROUP_PREFIX}{pid}")),
                        format!("legacy sandbox {sandbox_id}, workspace {}", workspace.id),
                    );
                }
            }
        }
        Ok(ownership)
    })
}

pub(super) fn remove_empty_workspace_cgroups(
    root: &Path,
    ownership: &CgroupOwnership,
) -> io::Result<usize> {
    remove_empty_workspace_cgroups_using(root, ownership, |path| fs::remove_dir(path))
}

fn remove_empty_workspace_cgroups_using(
    root: &Path,
    ownership: &CgroupOwnership,
    remove_dir: impl Fn(&Path) -> io::Result<()>,
) -> io::Result<usize> {
    let mut removed = 0;
    loop {
        let mut candidates = empty_workspace_cgroups(root)?
            .into_iter()
            .filter(|path| {
                let sandbox_owned = ownership
                    .sandbox_paths
                    .iter()
                    .any(|sandbox_path| path.starts_with(sandbox_path));
                let workspace_tracked = ownership.workspace_paths.keys().any(|tracked| {
                    tracked == path || tracked.starts_with(path) || path.starts_with(tracked)
                });
                sandbox_owned && !workspace_tracked
            })
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            return Ok(removed);
        }

        candidates.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
        let mut removed_this_pass = 0;
        for path in candidates {
            match remove_dir(&path) {
                Ok(()) => {
                    removed += 1;
                    removed_this_pass += 1;
                }
                // A process or child cgroup can appear after the inventory.
                // Preserve it and let the next repair report the remaining state.
                Err(err)
                    if matches!(
                        err.kind(),
                        io::ErrorKind::DirectoryNotEmpty | io::ErrorKind::NotFound
                    ) => {}
                Err(err) => {
                    return Err(io::Error::new(
                        err.kind(),
                        format!("failed to remove managed cgroup {}: {err}", path.display()),
                    ));
                }
            }
        }
        if removed_this_pass == 0 {
            return Ok(removed);
        }
    }
}

fn empty_workspace_cgroups(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut workspaces = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let path = entry.path();
        if name.starts_with(WORKSPACE_CGROUP_PREFIX) {
            if cgroup_is_empty_leaf(&path)? {
                workspaces.push(path);
            }
        } else if name.starts_with(SANDBOX_CGROUP_PREFIX) {
            collect_workspace_cgroups(&path, &mut workspaces)?;
        }
    }
    Ok(workspaces)
}

fn collect_workspace_cgroups(parent: &Path, workspaces: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(parent)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let path = entry.path();
        let name = entry.file_name();
        if name.to_string_lossy().starts_with(WORKSPACE_CGROUP_PREFIX) {
            if cgroup_is_empty_leaf(&path)? {
                workspaces.push(path.clone());
            }
            collect_workspace_cgroups(&path, workspaces)?;
        } else if name.to_string_lossy().starts_with(SANDBOX_CGROUP_PREFIX) {
            collect_workspace_cgroups(&path, workspaces)?;
        }
    }
    Ok(())
}

fn cgroup_is_empty_leaf(path: &Path) -> io::Result<bool> {
    let procs = fs::read_to_string(path.join("cgroup.procs"))?;
    if !procs.trim().is_empty() {
        return Ok(false);
    }
    for entry in fs::read_dir(path)? {
        if entry?.file_type()?.is_dir() {
            return Ok(false);
        }
    }
    Ok(true)
}

fn is_safe_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

#[cfg(test)]
#[path = "../../tests/src/doctor/cgroups.rs"]
mod tests;
