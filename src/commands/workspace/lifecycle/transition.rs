//! Driving a workspace through its transitions: start, stop, destroy, status, stats.
//!
//! Each of these answers with the state change it made, so the printed output can
//! say what the workspace was before and what it is now.

use super::*;

pub(in crate::commands::workspace) fn run_workspace_start(
    ctx: &WorkspaceCommandContext<'_>,
    args: WorkspaceTargetArgs,
) -> Result<()> {
    tracing::info!(
        "starting workspace '{}' in sandbox '{}'...",
        args.workspace,
        args.sandbox
    );
    let response = send_managed(
        ctx.socket,
        "workspace.start",
        json!({
            "sandbox": args.sandbox,
            "workspace": args.workspace,
        }),
    )?;
    println!("started workspace");
    print_state_transition(&response);
    print_operation_id();
    Ok(())
}

pub(in crate::commands::workspace) fn run_workspace_stop(
    ctx: &WorkspaceCommandContext<'_>,
    args: WorkspaceTargetArgs,
) -> Result<()> {
    tracing::info!(
        "stopping workspace '{}' in sandbox '{}'...",
        args.workspace,
        args.sandbox
    );
    let response = send_managed(
        ctx.socket,
        "workspace.stop",
        json!({
            "sandbox": args.sandbox,
            "workspace": args.workspace,
        }),
    )?;
    println!("stopped workspace");
    print_state_transition(&response);
    print_operation_id();
    // The daemon only reports a stop as successful once every mandatory host
    // resource has been verified released, so the certificate is the evidence
    // behind that success rather than a restatement of it.
    if let Some(certificate) = response.get("certificate") {
        let certificate: crate::workspace::WorkspaceCleanupCertificate =
            serde_json::from_value(certificate.clone())?;
        println!(
            "cleanup verified: runtime exited, cgroup removed, mounts released, loop device detached, runtime files removed, network and ports released"
        );
        if !certificate.is_complete() {
            bail!(
                "workspace '{}' cleanup is incomplete: {}",
                certificate.workspace_id,
                certificate.failure_summary()
            );
        }
    }
    Ok(())
}

pub(in crate::commands::workspace) fn run_workspace_destroy(
    ctx: &WorkspaceCommandContext<'_>,
    args: WorkspaceDestroyArgs,
) -> Result<()> {
    let WorkspaceDestroyArgs {
        sandbox,
        workspace,
        force,
    } = args;
    tracing::info!(
        "destroying workspace '{}' in sandbox '{}'{}...",
        workspace,
        sandbox,
        if force { " (force)" } else { "" }
    );
    let response = send_managed(
        ctx.socket,
        "workspace.destroy",
        json!({
            "sandbox": sandbox,
            "workspace": workspace,
            "force": force,
        }),
    )?;
    let report: crate::workspace::WorkspaceDestroyReport =
        serde_json::from_value(response.clone())?;
    println!("destroyed workspace {}", report.workspace_id);
    print_state_transition(&response);
    print_operation_id();
    report_retained_resources(
        report
            .retained
            .iter()
            .map(|item| format!("{}: {}", item.resource, item.detail)),
    );
    // The destroy certificate is the evidence that the host is clean, not just
    // that the files and the registry record are gone.
    if !report.certificate.is_complete() {
        eprintln!(
            "warning: workspace {} may have left resources behind: {}",
            report.workspace_id,
            report.certificate.failure_summary()
        );
    }
    Ok(())
}

pub(in crate::commands::workspace) fn run_workspace_status(
    ctx: &WorkspaceCommandContext<'_>,
    args: WorkspaceTargetArgs,
) -> Result<()> {
    let response = send_managed(
        ctx.socket,
        "workspace.status",
        json!({
            "sandbox": args.sandbox,
            "workspace": args.workspace,
        }),
    )?;
    let status: crate::workspace::WorkspaceStatusReport = serde_json::from_value(response)?;
    display::print_workspace_status(&status);
    Ok(())
}

pub(in crate::commands::workspace) fn run_workspace_stats(
    ctx: &WorkspaceCommandContext<'_>,
    args: WorkspaceTargetOrLocalArgs,
) -> Result<()> {
    let (sandbox, workspace) =
        resolve_workspace_target_from_optional(ctx, &args.target, args.workspace.as_deref())?;
    let response = send_managed(
        ctx.socket,
        "workspace.stats",
        json!({
            "sandbox": sandbox,
            "workspace": workspace,
        }),
    )?;
    let status: crate::workspace::WorkspaceStatsReport = serde_json::from_value(response)?;
    display::print_workspace_stats(&status);
    Ok(())
}
