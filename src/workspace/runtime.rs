use std::path::Path;

use anyhow::{anyhow, bail, Result};

use crate::registry::with_registry;
use crate::sandbox::resolve_sandbox_id;

use super::control::resolve_workspace_id;
use super::session;
use super::types::{WorkspaceRuntimeInfo, WorkspaceStatus};

pub fn workspace_runtime_info(
    state_dir: &Path,
    sandbox_selector: &str,
    workspace_selector: &str,
) -> Result<WorkspaceRuntimeInfo> {
    with_registry(state_dir, |registry| {
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

        if workspace.status != WorkspaceStatus::Running {
            bail!(
                "workspace '{}' is stopped; start workspace first",
                workspace.id
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

        Ok(WorkspaceRuntimeInfo {
            sandbox_id: workspace.sandbox_id.clone(),
            workspace_id: workspace.id.clone(),
            workspace_name: workspace.name.clone(),
            runtime_pid,
            runtime_starttime_ticks: starttime_ticks,
            sandbox_rootfs_path: workspace.sandbox_rootfs_path.clone(),
        })
    })
}
