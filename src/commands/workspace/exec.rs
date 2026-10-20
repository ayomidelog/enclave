use super::*;
use crate::cli::WorkspaceCancelArgs;

pub(super) fn run_workspace_exec(
    ctx: &WorkspaceCommandContext<'_>,
    args: WorkspaceExecArgs,
) -> Result<()> {
    enter::run_workspace_exec_direct(ctx, args)
}

/// Stop the command a workspace is running right now.
///
/// Exits 0 when nothing was running, because the caller asked for the workspace to
/// be free and it is. Reports whether something was actually stopped so a script
/// can tell the two apart.
pub(super) fn run_workspace_cancel(
    ctx: &WorkspaceCommandContext<'_>,
    args: WorkspaceCancelArgs,
) -> Result<()> {
    let response = send_managed(
        ctx.socket,
        "workspace.exec.cancel",
        serde_json::json!({
            "sandbox": args.sandbox_id,
            "workspace": args.workspace_id,
        }),
    )?;
    let cancelled = response
        .get("cancelled")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if cancelled {
        println!("cancelled the running command");
    } else {
        println!("no command was running");
    }
    Ok(())
}

pub(super) fn resolve_workspace_target_from_optional(
    _ctx: &WorkspaceCommandContext<'_>,
    first: &str,
    second: Option<&str>,
) -> Result<(String, String)> {
    if let Some(workspace) = second {
        return Ok((first.to_string(), workspace.to_string()));
    }

    let cwd = std::env::current_dir()?;
    let enclavefile_path = crate::enclavefile::find_enclavefile(&cwd).ok_or_else(|| {
        anyhow::anyhow!(
            "no Enclavefile found in {}. provide <sandbox> <workspace> or run from a project directory.",
            cwd.display()
        )
    })?;
    let enclavefile = crate::enclavefile::load_enclavefile(&enclavefile_path)?;

    if let Some(ws) = enclavefile.workspace.get(first) {
        return Ok((enclavefile.sandbox.name, ws.name.clone()));
    }

    let matching_names: Vec<String> = enclavefile
        .workspace
        .values()
        .filter(|ws| ws.name == first)
        .map(|ws| ws.name.clone())
        .collect();
    match matching_names.len() {
        1 => return Ok((enclavefile.sandbox.name, matching_names[0].clone())),
        n if n > 1 => {
            bail!(
                "workspace name '{}' is ambiguous in Enclavefile at {}. Multiple workspaces share this name; please specify <sandbox> <workspace> explicitly.",
                first,
                enclavefile_path.display()
            );
        }
        _ => {}
    }

    bail!(
        "workspace '{}' not found in Enclavefile at {}",
        first,
        enclavefile_path.display()
    );
}
