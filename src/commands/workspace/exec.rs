use super::*;

pub(super) fn run_workspace_exec(
    ctx: &WorkspaceCommandContext<'_>,
    args: WorkspaceExecArgs,
) -> Result<()> {
    enter::run_workspace_exec_direct(ctx, args)
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
