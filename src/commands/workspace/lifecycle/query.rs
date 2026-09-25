//! The workspace commands that read rather than change: list, remove, and wipe.
//!
//! They are grouped together because they answer a question or release a record
//! without driving a runtime, which is what the modules beside this one do.

use super::*;

pub(in crate::commands::workspace) fn run_workspace_list(
    ctx: &WorkspaceCommandContext<'_>,
    args: WorkspaceListArgs,
) -> Result<()> {
    let response = send_managed(
        ctx.socket,
        "workspace.list",
        json!({
            "sandbox_id": args.sandbox_id,
        }),
    )?;
    if args.sandbox_id.is_some() {
        let workspaces: Vec<WorkspaceListItem> = serde_json::from_value(response)?;
        return display::print_workspace_list_by_sandbox(workspaces);
    }
    let workspaces: Vec<WorkspaceMetadata> = serde_json::from_value(response)?;
    display::print_workspace_list_all(workspaces)
}

pub(in crate::commands::workspace) fn run_workspace_remove(
    ctx: &WorkspaceCommandContext<'_>,
    args: WorkspaceRemoveArgs,
) -> Result<()> {
    tracing::info!(
        "removing workspace '{}' from sandbox '{}'...",
        args.workspace_id,
        args.sandbox_id
    );
    let response = send_managed(
        ctx.socket,
        "workspace.remove",
        json!({
            "sandbox_id": args.sandbox_id,
            "workspace_id": args.workspace_id,
        }),
    )?;
    println!("removed workspace");
    print_state_transition(&response);
    print_operation_id();
    Ok(())
}

pub(in crate::commands::workspace) fn run_workspace_wipe(
    ctx: &WorkspaceCommandContext<'_>,
    args: WorkspaceWipeArgs,
) -> Result<()> {
    daemon::ensure_daemon_running_for_action(ctx.socket, "workspace.wipe")?;
    let response = send(ctx.socket, "workspace.list", json!({}))?;
    let workspaces: Vec<WorkspaceMetadata> = serde_json::from_value(response)?;
    if workspaces.is_empty() {
        println!("no workspaces");
        return Ok(());
    }

    if !confirm_destructive_action(
        &format!(
            "this will permanently delete all {} workspaces across all sandboxes.{}",
            workspaces.len(),
            if args.force {
                " force mode removes the records even when host resources cannot be released."
            } else {
                ""
            }
        ),
        "delete all workspace",
    )? {
        println!("aborted");
        return Ok(());
    }

    let response = send_managed(ctx.socket, "workspace.wipe", json!({ "force": args.force }))?;
    let report: crate::workspace::BatchDestroyReport = serde_json::from_value(response)?;
    println!("deleted {} workspaces", report.removed.len());
    report_retained_resources(report.retained.iter().flat_map(|(workspace, resources)| {
        resources
            .iter()
            .map(move |item| format!("{workspace}/{}: {}", item.resource, item.detail))
    }));
    Ok(())
}
