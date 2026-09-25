use anyhow::{anyhow, Result};

use crate::registry::with_registry;
use crate::sandbox::{resolve_sandbox_id, SandboxMetadata, SandboxStatus};
use crate::workspace::types::WorkspaceMetadata;

use super::control::{resolve_workspace_id, workspace_runtime_is_active};

pub(crate) fn workspace_cgroup_name(sandbox_id: &str, workspace_id: &str) -> String {
    format!("enclave-ws-{sandbox_id}-{workspace_id}")
}

/// Absolute path of the cgroup that holds a workspace's processes.
pub(crate) fn workspace_cgroup_path(sandbox_id: &str, workspace_id: &str) -> std::path::PathBuf {
    std::path::PathBuf::from("/sys/fs/cgroup")
        .join(crate::sandbox::cgroup::sandbox_cgroup_name(sandbox_id))
        .join(workspace_cgroup_name(sandbox_id, workspace_id))
}

/// Path of the workspace cgroup, when it already exists on this host.
///
/// A workspace started before the cgroup was introduced has no cgroup to attach
/// to, so callers that spawn helpers into the workspace run them without one
/// instead of failing.
pub(crate) fn existing_workspace_cgroup_path(
    sandbox_id: &str,
    workspace_id: &str,
) -> Option<String> {
    let path = workspace_cgroup_path(sandbox_id, workspace_id);
    path.is_dir().then(|| path.to_string_lossy().into_owned())
}

pub(crate) fn legacy_workspace_cgroup_name(pid: u32) -> String {
    format!("enclave-ws-{pid}")
}

pub(crate) fn sync_workspace_runtime_limits(
    state_dir: &std::path::Path,
    sandbox_selector: &str,
    workspace_selector: &str,
) -> Result<WorkspaceMetadata> {
    let (sandbox, workspace) = with_registry(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, sandbox_selector)?;
        let sandbox = registry
            .sandboxes
            .get(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_selector))?;
        let workspace_id = resolve_workspace_id(sandbox, workspace_selector)?;
        let workspace = sandbox
            .workspaces
            .get(&workspace_id)
            .cloned()
            .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_selector))?;
        Ok((sandbox.metadata.clone(), workspace))
    })?;

    if workspace_runtime_is_active(&workspace) {
        let pid = workspace
            .runtime_pid
            .expect("active workspace has a runtime pid");
        apply_workspace_runtime_constraints(&sandbox, &workspace, pid)?;
    }

    Ok(workspace)
}

pub(crate) fn sync_sandbox_runtime_limits(
    state_dir: &std::path::Path,
    sandbox_selector: &str,
) -> Result<()> {
    let (sandbox, workspaces) = with_registry(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, sandbox_selector)?;
        let sandbox = registry
            .sandboxes
            .get(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_selector))?;
        let workspaces = sandbox.workspaces.values().cloned().collect::<Vec<_>>();
        Ok((sandbox.metadata.clone(), workspaces))
    })?;

    if sandbox.status != SandboxStatus::Running {
        return Ok(());
    }

    if workspaces
        .iter()
        .all(|workspace| !workspace_runtime_is_active(workspace))
    {
        let sandbox_config = build_sandbox_cgroup_config(&sandbox)?;
        if crate::sandbox::cgroup::is_cgroup_v2_available() {
            if sandbox_config.has_limits() {
                crate::sandbox::cgroup::ensure_sandbox_cgroup(
                    &crate::sandbox::cgroup::sandbox_cgroup_name(&sandbox.id),
                    &sandbox_config,
                    true,
                )?;
            } else {
                super::control::remove_sandbox_cgroup(&sandbox);
            }
        }
        return Ok(());
    }

    for workspace in workspaces {
        if !workspace_runtime_is_active(&workspace) {
            continue;
        }
        let pid = workspace
            .runtime_pid
            .expect("active workspace has a runtime pid");
        apply_workspace_runtime_constraints(&sandbox, &workspace, pid)?;
    }

    Ok(())
}

pub(super) fn apply_workspace_runtime_constraints(
    sandbox: &SandboxMetadata,
    workspace: &WorkspaceMetadata,
    pid: u32,
) -> Result<()> {
    ensure_workspace_cgroup_hierarchy(sandbox, workspace, pid)
}

fn ensure_workspace_cgroup_hierarchy(
    sandbox: &SandboxMetadata,
    workspace: &WorkspaceMetadata,
    pid: u32,
) -> Result<()> {
    let sandbox_config = build_sandbox_cgroup_config(sandbox)?;
    let workspace_config = build_workspace_cgroup_config(workspace)?;
    let cgroup_v2_available = crate::sandbox::cgroup::is_cgroup_v2_available();

    if sandbox.limits.has_limits() && !cgroup_v2_available {
        return Err(crate::error::coded(
            crate::error::ErrorCode::Unsupported,
            format!(
                "sandbox '{}' declares resource limits but cgroup v2 is unavailable on this host",
                sandbox.id
            ),
        ));
    }
    if workspace.limits.cpu_percent_requires_cgroup() && !cgroup_v2_available {
        return Err(crate::error::coded(
            crate::error::ErrorCode::Unsupported,
            format!(
                "workspace '{}' declares cpu_percent but cgroup v2 is unavailable on this host",
                workspace.id
            ),
        ));
    }
    // Keep a cgroup hierarchy even for unlimited workspaces. It provides the
    // process boundary required by warm pause/resume and fast cgroup.kill
    // shutdown; resource limits remain unlimited through the configured `max`
    // values.
    if !cgroup_v2_available {
        return Ok(());
    }

    let sandbox_path = crate::sandbox::cgroup::ensure_sandbox_cgroup(
        &crate::sandbox::cgroup::sandbox_cgroup_name(&sandbox.id),
        &sandbox_config,
        true,
    )?
    .ok_or_else(|| anyhow!("failed to prepare sandbox cgroup for '{}'", sandbox.id))?;
    let workspace_name = workspace_cgroup_name(&sandbox.id, &workspace.id);
    if let Some(workspace_path) = crate::sandbox::cgroup::ensure_workspace_cgroup(
        &sandbox_path,
        &workspace_name,
        &workspace_config,
        true,
    )? {
        crate::sandbox::cgroup::add_process_to_cgroup(&workspace_path, pid)?;
    }
    if let Err(err) =
        crate::sandbox::cgroup::remove_workspace_cgroup(&legacy_workspace_cgroup_name(pid))
    {
        tracing::debug!(
            "legacy workspace cgroup cleanup skipped for {}: {err:#}",
            workspace.id
        );
    }
    Ok(())
}

fn build_sandbox_cgroup_config(
    sandbox: &SandboxMetadata,
) -> Result<crate::sandbox::cgroup::CgroupConfig> {
    crate::sandbox::cgroup::CgroupConfig::from_limits(
        sandbox.limits.memory_bytes,
        sandbox.limits.cpu_percent,
        sandbox.limits.max_processes,
    )
}

fn build_workspace_cgroup_config(
    workspace: &WorkspaceMetadata,
) -> Result<crate::sandbox::cgroup::CgroupConfig> {
    crate::sandbox::cgroup::CgroupConfig::from_limits(
        workspace.limits.memory_bytes,
        workspace.limits.cpu_percent,
        workspace.limits.max_processes,
    )
}
