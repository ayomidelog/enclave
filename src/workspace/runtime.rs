use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};

use crate::registry::with_registry;
use crate::sandbox::resolve_sandbox_id;

use super::control::resolve_workspace_id;
use super::session;
use super::types::WorkspaceRuntimeInfo;

pub fn workspace_runtime_info(
    state_dir: &Path,
    sandbox_selector: &str,
    workspace_selector: &str,
) -> Result<WorkspaceRuntimeInfo> {
    let (sandbox, workspace) = with_registry(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, sandbox_selector)?;
        let sandbox = registry
            .sandboxes
            .get(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_id))?;
        let workspace_id = resolve_workspace_id(sandbox, workspace_selector)?;
        let workspace = sandbox
            .workspaces
            .get(&workspace_id)
            .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_id))?;
        Ok((sandbox.metadata.clone(), workspace.clone()))
    })?;

    if !workspace.status.is_running() {
        bail!(
            "workspace '{}' is {}; start workspace first",
            workspace.id,
            workspace.status.as_str()
        );
    }

    let runtime_pid = workspace.runtime_pid.ok_or_else(|| {
        anyhow!(
            "workspace '{}' has no runtime pid; restart workspace",
            workspace.id
        )
    })?;
    let starttime_ticks = workspace.runtime_starttime_ticks.ok_or_else(|| {
        anyhow!(
            "workspace '{}' has no runtime starttime; restart workspace",
            workspace.id
        )
    })?;
    if !session::process_matches(runtime_pid, Some(starttime_ticks)) {
        bail!(
            "workspace '{}' runtime pid {} is not alive; restart workspace",
            workspace.id,
            runtime_pid
        );
    }
    if !session::namespace_refs_match_runtime(&workspace, runtime_pid) {
        bail!(
            "workspace '{}' namespace references are stale; restart workspace",
            workspace.id
        );
    }

    // Callers spawn helpers into the workspace through this response, so the
    // cgroup has to exist before they attach to it. Re-asserting it here also
    // repairs workspaces that were started before the cgroup was introduced.
    let cgroup_context = format!(
        "failed to prepare the cgroup for workspace '{}'",
        workspace.id
    );
    super::runtime_limits::apply_workspace_runtime_constraints(&sandbox, &workspace, runtime_pid)
        .context(cgroup_context)?;
    let cgroup_path = super::workspace_cgroup_path(&workspace.sandbox_id, &workspace.id);
    let cgroup_path = cgroup_path
        .is_dir()
        .then(|| cgroup_path.to_string_lossy().into_owned());

    Ok(WorkspaceRuntimeInfo {
        sandbox_id: workspace.sandbox_id.clone(),
        workspace_id: workspace.id.clone(),
        workspace_name: workspace.name.clone(),
        runtime_pid,
        runtime_starttime_ticks: starttime_ticks,
        sandbox_rootfs_path: workspace.sandbox_rootfs_path.clone(),
        cgroup_path,
    })
}
