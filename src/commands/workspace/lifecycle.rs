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
            if let Some(hint) =
                existing_workspace_create_hint(ctx.socket, &sandbox_id, &workspace_name)?
            {
                bail!("{hint}");
            }
            return Err(err);
        }
    };
    let metadata: WorkspaceMetadata = serde_json::from_value(response)?;
    println!(
        "created and started workspace {} in sandbox {}",
        metadata.id, metadata.sandbox_id
    );
    println!("workspace path {}", metadata.workspace_path);
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

pub(super) fn run_workspace_list(
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

pub(super) fn run_workspace_remove(
    ctx: &WorkspaceCommandContext<'_>,
    args: WorkspaceRemoveArgs,
) -> Result<()> {
    tracing::info!(
        "removing workspace '{}' from sandbox '{}'...",
        args.workspace_id,
        args.sandbox_id
    );
    send_managed(
        ctx.socket,
        "workspace.remove",
        json!({
            "sandbox_id": args.sandbox_id,
            "workspace_id": args.workspace_id,
        }),
    )?;
    println!("removed workspace");
    print_operation_id();
    Ok(())
}

pub(super) fn run_workspace_wipe(
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

pub(super) fn run_workspace_start(
    ctx: &WorkspaceCommandContext<'_>,
    args: WorkspaceTargetArgs,
) -> Result<()> {
    tracing::info!(
        "starting workspace '{}' in sandbox '{}'...",
        args.workspace,
        args.sandbox
    );
    send_managed(
        ctx.socket,
        "workspace.start",
        json!({
            "sandbox": args.sandbox,
            "workspace": args.workspace,
        }),
    )?;
    println!("started workspace");
    print_operation_id();
    Ok(())
}

pub(super) fn run_workspace_stop(
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

pub(super) fn run_workspace_destroy(
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
    let report: crate::workspace::WorkspaceDestroyReport = serde_json::from_value(response)?;
    println!("destroyed workspace {}", report.workspace_id);
    print_operation_id();
    report_retained_resources(
        report
            .retained
            .iter()
            .map(|item| format!("{}: {}", item.resource, item.detail)),
    );
    Ok(())
}

pub(super) fn run_workspace_status(
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

pub(super) fn run_workspace_stats(
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
