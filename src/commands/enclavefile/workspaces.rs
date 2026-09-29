//! Bringing the Enclavefile workspaces up as one batch.

use super::*;

pub(super) fn bring_up_workspaces(socket: &Path, ef: &Enclavefile, ef_path: &Path) -> Result<()> {
    let definitions = ef
        .workspace
        .iter()
        .enumerate()
        .map(|(index, (key, workspace))| {
            let workspace_dir = match (
                workspace.workspace_dir.as_deref(),
                workspace.path.as_deref(),
            ) {
                (Some(raw), _) => Some(
                    crate::enclavefile::resolve_workspace_host_dir(ef_path, raw, "workspace_dir")
                        .with_context(|| format!("failed to resolve workspace '{}'", key))?,
                ),
                (None, Some(raw)) => Some(
                    crate::enclavefile::resolve_workspace_host_dir(ef_path, raw, "path")
                        .with_context(|| format!("failed to resolve workspace '{}'", key))?,
                ),
                (None, None) => None,
            };
            Ok::<_, anyhow::Error>((index, key.as_str(), workspace, workspace_dir))
        })
        .collect::<Result<Vec<_>>>()?;

    start_workspace_definitions_bulk(socket, &ef.sandbox.name, &definitions)?;

    Ok(())
}

pub(super) fn start_workspace_definitions_bulk(
    socket: &Path,
    sandbox_name: &str,
    definitions: &[(
        usize,
        &str,
        &crate::enclavefile::WorkspaceSection,
        Option<String>,
    )],
) -> Result<()> {
    if definitions.is_empty() {
        return Ok(());
    }
    let workspaces = definitions
        .iter()
        .map(|(_, _, workspace, workspace_dir)| {
            json!({
                "sandbox_id": sandbox_name,
                "name": workspace.name,
                "path": workspace_dir,
                "cpu_seconds": workspace.cpu_seconds,
                "cpu_percent": workspace.cpu_percent,
                "memory_mb": workspace.memory_mb,
                "max_procs": workspace.max_procs,
                "max_open_files": workspace.max_open_files,
                "disk_mb": workspace.disk_mb,
                "clear_tmp_on_restart": workspace.clear_tmp_on_restart,
                "auth": workspace.auth,
                "owner": workspace.owner,
                "env_tokens": workspace.env_tokens,
                "ports": workspace.ports,
                "run": workspace.run,
            })
        })
        .collect::<Vec<_>>();
    let result = send_managed(
        socket,
        "workspace.start_many",
        json!({ "workspaces": workspaces }),
    )?;
    let failures = result
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| {
            item.get("ok")
                .and_then(|value| value.as_bool())
                .is_some_and(|ok| !ok)
        })
        .map(|item| {
            item.get("error")
                .and_then(|error| error.as_str())
                .unwrap_or("workspace startup failed")
                .to_string()
        })
        .collect::<Vec<_>>();
    if !failures.is_empty() {
        bail!(
            "one or more workspaces failed to start: {}",
            failures.join("; ")
        );
    }
    Ok(())
}
