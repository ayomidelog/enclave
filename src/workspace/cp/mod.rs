mod path;
mod stream;

use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Instant;

use anyhow::{anyhow, bail, Result};

use crate::registry::with_registry;
use crate::sandbox::resolve_sandbox_id;

use super::control::resolve_workspace_id;
use super::session;
use super::types::{WorkspaceCpResult, WorkspaceMetadata, WorkspaceStatus};

use self::path::{
    validate_direction_paths, validate_host_destination, validate_host_source, Direction,
};
use self::stream::{
    run_host_to_workspace, run_workspace_to_host, workspace_path_has_symlink,
    workspace_path_is_directory,
};

pub fn copy_workspace_path(
    state_dir: &Path,
    sandbox_selector: &str,
    workspace_selector: &str,
    src: &str,
    dst: &str,
    direction: &str,
) -> Result<WorkspaceCpResult> {
    copy_workspace_path_with_connection(
        state_dir,
        sandbox_selector,
        workspace_selector,
        src,
        dst,
        direction,
        None,
    )
}

pub(crate) fn copy_workspace_path_with_connection(
    state_dir: &Path,
    sandbox_selector: &str,
    workspace_selector: &str,
    src: &str,
    dst: &str,
    direction: &str,
    client_stream: Option<&UnixStream>,
) -> Result<WorkspaceCpResult> {
    let direction = Direction::parse(direction)?;
    validate_direction_paths(src, dst, direction)?;
    let workspace = load_workspace(state_dir, sandbox_selector, workspace_selector)?;
    ensure_workspace_running(&workspace)?;

    if direction == Direction::HostToWorkspace {
        validate_host_source(src)?;
    } else {
        validate_host_destination(dst)?;
    }

    let started = Instant::now();
    let transfer = crate::workspace::with_workspace_storage_mounted(&workspace, || {
        if direction == Direction::HostToWorkspace {
            workspace_path_has_symlink(&workspace, dst, client_stream)?;
        }
        let source_is_dir = match direction {
            Direction::HostToWorkspace => Path::new(src).is_dir(),
            Direction::WorkspaceToHost => {
                workspace_path_is_directory(&workspace, src, client_stream)?
            }
        };
        let destination_is_dir = match direction {
            Direction::HostToWorkspace => {
                workspace_path_is_directory(&workspace, dst, client_stream)?
            }
            Direction::WorkspaceToHost => Path::new(dst).is_dir(),
        };
        if dst.ends_with('/') && !destination_is_dir && !source_is_dir {
            bail!("destination '{}' is not an existing directory", dst);
        }

        let source_name = path::source_name(src)?;
        let destination = path::destination_plan(dst, &source_name, destination_is_dir)?;
        match direction {
            Direction::HostToWorkspace => {
                run_host_to_workspace(&workspace, src, &destination, &source_name, client_stream)
            }
            Direction::WorkspaceToHost => {
                run_workspace_to_host(&workspace, src, &destination, &source_name, client_stream)
            }
        }
    })?;

    Ok(WorkspaceCpResult {
        workspace_id: workspace.id,
        workspace_name: workspace.name,
        logical_bytes: transfer.logical_bytes,
        elapsed_ms: started.elapsed().as_millis(),
    })
}

fn load_workspace(
    state_dir: &Path,
    sandbox_selector: &str,
    workspace_selector: &str,
) -> Result<WorkspaceMetadata> {
    with_registry(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, sandbox_selector)?;
        let sandbox = registry
            .sandboxes
            .get(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_id))?;
        let workspace_id = resolve_workspace_id(sandbox, workspace_selector)?;
        sandbox
            .workspaces
            .get(&workspace_id)
            .cloned()
            .ok_or_else(|| {
                anyhow!(
                    "workspace '{}' not found in sandbox '{}'",
                    workspace_id,
                    sandbox_id
                )
            })
    })
}

fn ensure_workspace_running(workspace: &WorkspaceMetadata) -> Result<()> {
    if workspace.status != WorkspaceStatus::Running {
        bail!(
            "workspace '{}' is stopped; start workspace first",
            workspace.id
        );
    }
    let pid = workspace.runtime_pid.ok_or_else(|| {
        anyhow!(
            "workspace '{}' has no runtime pid; restart workspace",
            workspace.id
        )
    })?;
    if !session::process_matches(pid, workspace.runtime_starttime_ticks) {
        bail!(
            "workspace '{}' runtime pid {} is not alive; restart workspace",
            workspace.id,
            pid
        );
    }
    Ok(())
}

#[cfg(test)]
#[path = "../../../tests/src/workspace/cp.rs"]
mod tests;
