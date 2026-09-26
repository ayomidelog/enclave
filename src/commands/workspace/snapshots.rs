use super::*;

pub(super) fn run_workspace_snapshot(
    ctx: &WorkspaceCommandContext<'_>,
    args: WorkspaceSnapshotArgs,
) -> Result<()> {
    tracing::info!(
        "creating snapshot for workspace '{}' in sandbox '{}'...",
        args.workspace,
        args.sandbox
    );
    let response = send_managed(
        ctx.socket,
        "workspace.snapshot",
        json!({
            "sandbox": args.sandbox,
            "workspace": args.workspace,
            "name": args.name,
        }),
    )?;
    let snapshot: WorkspaceSnapshotInfo = serde_json::from_value(response)?;
    println!("snapshot created: {}", snapshot.name);
    println!("created_at: {}", snapshot.created_at);
    println!("path: {}", snapshot.path);
    Ok(())
}

pub(super) fn run_workspace_snapshot_list(
    ctx: &WorkspaceCommandContext<'_>,
    args: WorkspaceTargetArgs,
) -> Result<()> {
    let response = send_managed(
        ctx.socket,
        "workspace.snapshot.list",
        json!({
            "sandbox": args.sandbox,
            "workspace": args.workspace,
        }),
    )?;
    let snapshots: Vec<WorkspaceSnapshotInfo> = serde_json::from_value(response)?;
    display::print_snapshot_list(snapshots)
}

pub(super) fn run_workspace_restore(
    ctx: &WorkspaceCommandContext<'_>,
    args: WorkspaceRestoreArgs,
) -> Result<()> {
    tracing::info!(
        "restoring workspace '{}' in sandbox '{}' from snapshot '{}'...",
        args.workspace,
        args.sandbox,
        args.snapshot
    );
    send_managed(
        ctx.socket,
        "workspace.restore",
        json!({
            "sandbox": args.sandbox,
            "workspace": args.workspace,
            "snapshot": args.snapshot,
        }),
    )?;
    println!("workspace restored");
    Ok(())
}

pub(super) fn run_workspace_snapshot_gc(
    ctx: &WorkspaceCommandContext<'_>,
    args: WorkspaceSnapshotGcArgs,
) -> Result<()> {
    tracing::info!(
        "garbage collecting snapshots for workspace '{}' in sandbox '{}' (keeping {})...",
        args.workspace,
        args.sandbox,
        args.keep
    );
    let response = send_managed(
        ctx.socket,
        "workspace.snapshot.gc",
        json!({
            "sandbox": args.sandbox,
            "workspace": args.workspace,
            "keep": args.keep,
        }),
    )?;
    let removed: Vec<WorkspaceSnapshotInfo> = serde_json::from_value(response)?;
    if removed.is_empty() {
        println!("no snapshots to remove");
    } else {
        println!("removed {} snapshot(s):", removed.len());
        for snapshot in &removed {
            println!("  {} ({})", snapshot.name, snapshot.created_at);
        }
    }
    Ok(())
}

pub(super) fn run_workspace_snapshot_export(
    ctx: &WorkspaceCommandContext<'_>,
    args: WorkspaceSnapshotExportArgs,
) -> Result<()> {
    tracing::info!(
        "exporting snapshot '{}' for workspace '{}' in sandbox '{}'...",
        args.snapshot,
        args.workspace,
        args.sandbox
    );
    let response = send_managed(
        ctx.socket,
        "workspace.snapshot.export",
        json!({
            "sandbox": args.sandbox,
            "workspace": args.workspace,
            "snapshot": args.snapshot,
            "output": args.output,
        }),
    )?;
    let archive: WorkspaceSnapshotArchiveInfo = serde_json::from_value(response)?;
    println!("snapshot exported: {}", archive.name);
    println!("archive: {}", archive.archive_path);
    Ok(())
}

pub(super) fn run_workspace_snapshot_import(
    ctx: &WorkspaceCommandContext<'_>,
    args: WorkspaceSnapshotImportArgs,
) -> Result<()> {
    tracing::info!(
        "importing snapshot archive '{}' for workspace '{}' in sandbox '{}'...",
        args.archive.display(),
        args.workspace,
        args.sandbox
    );
    let response = send_managed(
        ctx.socket,
        "workspace.snapshot.import",
        json!({
            "sandbox": args.sandbox,
            "workspace": args.workspace,
            "name": args.name,
            "replace": args.replace,
            "archive": args.archive,
        }),
    )?;
    let snapshot: WorkspaceSnapshotInfo = serde_json::from_value(response)?;
    println!("snapshot imported: {}", snapshot.name);
    println!("created_at: {}", snapshot.created_at);
    println!("path: {}", snapshot.path);
    Ok(())
}
