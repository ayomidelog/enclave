use anyhow::{bail, Result};

use crate::sandbox::SandboxMetadata;

use super::super::runtime_limits::{legacy_workspace_cgroup_name, workspace_cgroup_name};

/// Remove the cgroups a workspace owns.
///
/// The cgroup is removed even when no runtime pid was recorded: an interrupted
/// launch can create the cgroup before it records the pid. The legacy
/// pid-named cgroup is cleaned up only when a pid is known, because its name is
/// derived from one.
pub(crate) fn remove_workspace_cgroups(
    sandbox: &SandboxMetadata,
    workspace_id: &str,
    pid: Option<u32>,
) -> Result<()> {
    let workspace_name = workspace_cgroup_name(&sandbox.id, workspace_id);
    let sandbox_path = std::path::PathBuf::from("/sys/fs/cgroup")
        .join(crate::sandbox::cgroup::sandbox_cgroup_name(&sandbox.id))
        .join(&workspace_name);
    let mut errors = Vec::new();
    let mut attempts = vec![(
        sandbox_path.display().to_string(),
        crate::sandbox::cgroup::remove_cgroup_path(&sandbox_path),
    )];
    if let Some(pid) = pid {
        let legacy_name = legacy_workspace_cgroup_name(pid);
        attempts.push((
            format!("legacy {legacy_name}"),
            crate::sandbox::cgroup::remove_workspace_cgroup(&legacy_name),
        ));
    }
    for (label, result) in attempts {
        if let Err(error) = result {
            errors.push(format!("{label}: {error:#}"));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        bail!("workspace cgroup cleanup incomplete: {}", errors.join("; "))
    }
}
