use super::*;

pub(super) fn run_workspace_create(
    ctx: &WorkspaceCommandContext<'_>,
    args: WorkspaceCreateArgs,
) -> Result<()> {
    let sandbox_id = args.sandbox_id.clone();
    let workspace_name = args.name.clone();
    tracing::info!(
        "creating workspace '{}' in sandbox '{}'...",
        workspace_name,
        sandbox_id
    );
    let response = match send_managed(
        ctx.socket,
        "workspace.create",
        json!({
            "sandbox_id": sandbox_id.clone(),
            "name": workspace_name.clone(),
            "cpu_seconds": args.cpu_seconds,
            "cpu_percent": args.cpu_percent,
            "memory_mb": args.memory_mb,
            "max_procs": args.max_procs,
            "max_open_files": args.max_open_files,
            "disk_mb": args.disk_mb,
        }),
    ) {
        Ok(response) => response,
        Err(err) => {
            // The hint is only for the case it describes: the name was already
            // taken, which the daemon reports as a conflict. A create can also fail
            // after it has recorded the workspace — a runtime that never became
            // ready, say — and a workspace with that name exists afterwards too, so
            // asking only whether the name is present now would answer "already
            // exists" and hide the failure that actually happened.
            if crate::error::code_of(&err) == crate::error::ErrorCode::Conflict {
                if let Some(hint) =
                    existing_workspace_create_hint(ctx.socket, &sandbox_id, &workspace_name)?
                {
                    bail!("{hint}");
                }
            }
            return Err(err);
        }
    };
    let metadata: WorkspaceMetadata = serde_json::from_value(response.clone())?;
    println!(
        "created and started workspace {} in sandbox {}",
        metadata.id, metadata.sandbox_id
    );
    print_state_transition(&response);
    println!("workspace path {}", metadata.workspace_path);
    // Every other lifecycle command prints the operation id, and this one is the
    // first command an operator runs against a new workspace. Without it the
    // phases the daemon logged for this create cannot be tied to the command
    // that produced them, which is the correlation the id exists for.
    print_operation_id();
    Ok(())
}

pub(super) fn run_workspace_resize(
    ctx: &WorkspaceCommandContext<'_>,
    args: WorkspaceResizeArgs,
) -> Result<()> {
    let response = send_managed(
        ctx.socket,
        "workspace.resize",
        json!({
            "sandbox": args.sandbox,
            "workspace": args.workspace,
            "disk_mb": args.disk_mb,
        }),
    )?;
    let result: crate::workspace::WorkspaceResizeResult = serde_json::from_value(response)?;
    if result.previous_disk_bytes == result.new_disk_bytes {
        println!(
            "workspace {} already has a {} MiB disk allocation",
            result.workspace_name,
            result.new_disk_bytes / (1024 * 1024)
        );
    } else {
        println!(
            "resized workspace {} from {} MiB to {} MiB",
            result.workspace_name,
            result.previous_disk_bytes / (1024 * 1024),
            result.new_disk_bytes / (1024 * 1024)
        );
    }
    if result.restarted {
        println!("workspace restarted");
    }
    print_operation_id();
    Ok(())
}

fn existing_workspace_create_hint(
    socket: &Path,
    sandbox_id: &str,
    workspace_name: &str,
) -> Result<Option<String>> {
    let response = send(socket, "workspace.list", json!({"sandbox_id": sandbox_id}))?;
    let workspaces: Vec<WorkspaceListItem> = serde_json::from_value(response)?;
    if !workspaces
        .iter()
        .any(|item| item.name == workspace_name || item.id == workspace_name)
    {
        return Ok(None);
    }
    Ok(Some(format!(
        "workspace '{}' already exists. try `enclave workspace start {} {}` or `enclave workspace list --sandbox-id {}`.",
        workspace_name, sandbox_id, workspace_name, sandbox_id
    )))
}

mod query;
mod transition;

pub(super) use query::{run_workspace_list, run_workspace_remove, run_workspace_wipe};
pub(super) use transition::{
    run_workspace_destroy, run_workspace_start, run_workspace_stats, run_workspace_status,
    run_workspace_stop,
};
