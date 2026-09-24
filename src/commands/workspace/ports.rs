use super::*;

pub(super) fn run_workspace_port_command(
    ctx: &WorkspaceCommandContext<'_>,
    command: WorkspacePortCommands,
) -> Result<()> {
    match command {
        WorkspacePortCommands::Publish(args) => run_workspace_port_publish(ctx, args),
        WorkspacePortCommands::Unpublish(args) => run_workspace_port_unpublish(ctx, args),
        WorkspacePortCommands::List(args) => run_workspace_port_list(ctx, args),
    }
}

pub(super) fn run_workspace_port_publish(
    ctx: &WorkspaceCommandContext<'_>,
    args: WorkspacePortPublishArgs,
) -> Result<()> {
    let response = send_managed(
        ctx.socket,
        "workspace.port.publish",
        json!({
            "sandbox": args.sandbox,
            "workspace": args.workspace,
            "spec": args.spec,
        }),
    )?;
    let ports: Vec<PublishedPortStatus> = serde_json::from_value(response)?;
    display::print_workspace_ports(&ports);
    Ok(())
}

pub(super) fn run_workspace_port_unpublish(
    ctx: &WorkspaceCommandContext<'_>,
    args: WorkspacePortUnpublishArgs,
) -> Result<()> {
    let response = send_managed(
        ctx.socket,
        "workspace.port.unpublish",
        json!({
            "sandbox": args.sandbox,
            "workspace": args.workspace,
            "binding": args.binding,
        }),
    )?;
    let ports: Vec<PublishedPortStatus> = serde_json::from_value(response)?;
    display::print_workspace_ports(&ports);
    Ok(())
}

pub(super) fn run_workspace_port_list(
    ctx: &WorkspaceCommandContext<'_>,
    args: WorkspaceTargetArgs,
) -> Result<()> {
    let response = send_managed(
        ctx.socket,
        "workspace.port.list",
        json!({
            "sandbox": args.sandbox,
            "workspace": args.workspace,
        }),
    )?;
    let ports: Vec<PublishedPortStatus> = serde_json::from_value(response)?;
    display::print_workspace_ports(&ports);
    Ok(())
}
