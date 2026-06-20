pub(crate) mod display;
mod enter;

use std::io::Write;
use std::path::Path;
use std::thread;
use std::time::Duration;

use anyhow::{bail, Result};
use serde_json::json;

use crate::cli::{
    SnapshotCommands, WorkspaceCommands, WorkspaceCreateArgs, WorkspaceExecArgs, WorkspaceListArgs,
    WorkspaceLogsArgs, WorkspacePortCommands, WorkspacePortPublishArgs, WorkspacePortUnpublishArgs,
    WorkspaceRemoveArgs, WorkspaceRestoreArgs, WorkspaceSnapshotArgs, WorkspaceSnapshotGcArgs,
    WorkspaceTargetArgs, WorkspaceTargetOrLocalArgs,
};
use crate::workspace::{
    PublishedPortStatus, WorkspaceListItem, WorkspaceLogsResult, WorkspaceMetadata,
    WorkspaceSnapshotInfo,
};

use super::{confirm_destructive_action, send_managed};

struct WorkspaceCommandContext<'a> {
    socket: &'a Path,
}

const LOG_FOLLOW_POLL_INTERVAL: Duration = Duration::from_millis(500);
const LOG_FOLLOW_MAX_POLL_INTERVAL: Duration = Duration::from_secs(2);

pub(crate) fn run_workspace_command(socket: &Path, command: WorkspaceCommands) -> Result<()> {
    let ctx = WorkspaceCommandContext { socket };
    match command {
        WorkspaceCommands::Create(args) => run_workspace_create(&ctx, args),
        WorkspaceCommands::List(args) => run_workspace_list(&ctx, args),
        WorkspaceCommands::Remove(args) => run_workspace_remove(&ctx, args),
        WorkspaceCommands::Wipe => run_workspace_wipe(&ctx),
        WorkspaceCommands::Start(args) => run_workspace_start(&ctx, args),
        WorkspaceCommands::Stop(args) => run_workspace_stop(&ctx, args),
        WorkspaceCommands::Destroy(args) => run_workspace_destroy(&ctx, args),
        WorkspaceCommands::Status(args) => run_workspace_status(&ctx, args),
        WorkspaceCommands::Stats(args) => run_workspace_stats(&ctx, args),
        WorkspaceCommands::Enter(args) => enter::run_workspace_enter(&ctx, args),
        WorkspaceCommands::Logs(args) => run_workspace_logs(&ctx, args),
        WorkspaceCommands::Snapshot(args) => run_workspace_snapshot(&ctx, args),
        WorkspaceCommands::SnapshotList(args) => run_workspace_snapshot_list(&ctx, args),
        WorkspaceCommands::Restore(args) => run_workspace_restore(&ctx, args),
        WorkspaceCommands::SnapshotGc(args) => run_workspace_snapshot_gc(&ctx, args),
        WorkspaceCommands::Exec(args) => run_workspace_exec(&ctx, args),
        WorkspaceCommands::Run(args) => run_workspace_exec(&ctx, args),
        WorkspaceCommands::Port { command } => run_workspace_port_command(&ctx, command),
    }
}

pub(crate) fn run_snapshot_command(socket: &Path, command: SnapshotCommands) -> Result<()> {
    let ctx = WorkspaceCommandContext { socket };
    match command {
        SnapshotCommands::Create(args) => run_workspace_snapshot(&ctx, args),
        SnapshotCommands::List(args) => run_workspace_snapshot_list(&ctx, args),
        SnapshotCommands::Restore(args) => run_workspace_restore(&ctx, args),
    }
}

fn run_workspace_create(
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
            if let Some(hint) = existing_workspace_create_hint(&err, &sandbox_id, &workspace_name) {
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

fn run_workspace_port_command(
    ctx: &WorkspaceCommandContext<'_>,
    command: WorkspacePortCommands,
) -> Result<()> {
    match command {
        WorkspacePortCommands::Publish(args) => run_workspace_port_publish(ctx, args),
        WorkspacePortCommands::Unpublish(args) => run_workspace_port_unpublish(ctx, args),
        WorkspacePortCommands::List(args) => run_workspace_port_list(ctx, args),
    }
}

fn existing_workspace_create_hint(
    err: &anyhow::Error,
    sandbox_id: &str,
    workspace_name: &str,
) -> Option<String> {
    let msg = format!("{err:#}");
    if !msg.contains("already exists") {
        return None;
    }
    Some(format!(
        "workspace '{}' already exists. try `enclave workspace start {} {}` or `enclave workspace list --sandbox-id {}`.",
        workspace_name, sandbox_id, workspace_name, sandbox_id
    ))
}

fn run_workspace_list(ctx: &WorkspaceCommandContext<'_>, args: WorkspaceListArgs) -> Result<()> {
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

fn run_workspace_remove(
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
    Ok(())
}

fn run_workspace_wipe(ctx: &WorkspaceCommandContext<'_>) -> Result<()> {
    let response = send_managed(ctx.socket, "workspace.list", json!({}))?;
    let workspaces: Vec<WorkspaceMetadata> = serde_json::from_value(response)?;
    if workspaces.is_empty() {
        println!("no workspaces");
        return Ok(());
    }

    if !confirm_destructive_action(
        &format!(
            "this will permanently delete all {} workspaces across all sandboxes.",
            workspaces.len()
        ),
        "delete all workspace",
    )? {
        println!("aborted");
        return Ok(());
    }

    let total = workspaces.len();
    for workspace in workspaces {
        tracing::info!(
            "destroying workspace '{}' in sandbox '{}'...",
            workspace.id,
            workspace.sandbox_id
        );
        send_managed(
            ctx.socket,
            "workspace.destroy",
            json!({
                "sandbox": workspace.sandbox_id,
                "workspace": workspace.id,
            }),
        )?;
    }

    println!("deleted {total} workspaces");
    Ok(())
}

fn run_workspace_start(ctx: &WorkspaceCommandContext<'_>, args: WorkspaceTargetArgs) -> Result<()> {
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
    Ok(())
}

fn run_workspace_stop(ctx: &WorkspaceCommandContext<'_>, args: WorkspaceTargetArgs) -> Result<()> {
    tracing::info!(
        "stopping workspace '{}' in sandbox '{}'...",
        args.workspace,
        args.sandbox
    );
    send_managed(
        ctx.socket,
        "workspace.stop",
        json!({
            "sandbox": args.sandbox,
            "workspace": args.workspace,
        }),
    )?;
    println!("stopped workspace");
    Ok(())
}

fn run_workspace_destroy(
    ctx: &WorkspaceCommandContext<'_>,
    args: WorkspaceTargetArgs,
) -> Result<()> {
    tracing::info!(
        "destroying workspace '{}' in sandbox '{}'...",
        args.workspace,
        args.sandbox
    );
    send_managed(
        ctx.socket,
        "workspace.destroy",
        json!({
            "sandbox": args.sandbox,
            "workspace": args.workspace,
        }),
    )?;
    println!("destroyed workspace");
    Ok(())
}

fn run_workspace_status(
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

fn run_workspace_port_publish(
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

fn run_workspace_port_unpublish(
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

fn run_workspace_port_list(
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

fn run_workspace_logs(ctx: &WorkspaceCommandContext<'_>, args: WorkspaceLogsArgs) -> Result<()> {
    let (sandbox, workspace) =
        resolve_workspace_target_from_optional(ctx, &args.target, args.workspace.as_deref())?;
    let mut logs = fetch_workspace_logs(ctx, &sandbox, &workspace, args.tail)?;
    if logs.content.is_empty() {
        println!("no logs");
    } else {
        print!("{}", logs.content);
        std::io::stdout().flush()?;
    }
    if !args.follow {
        return Ok(());
    }

    let mut previous = if args.tail.is_some() {
        fetch_workspace_logs(ctx, &sandbox, &workspace, None)?.content
    } else {
        logs.content
    };
    let mut poll_interval = LOG_FOLLOW_POLL_INTERVAL;
    loop {
        thread::sleep(poll_interval);
        logs = fetch_workspace_logs(ctx, &sandbox, &workspace, None)?;
        if logs.content == previous {
            poll_interval = std::cmp::min(
                poll_interval.saturating_mul(2),
                LOG_FOLLOW_MAX_POLL_INTERVAL,
            );
            continue;
        }
        poll_interval = LOG_FOLLOW_POLL_INTERVAL;
        if logs.content.starts_with(&previous) {
            print!("{}", &logs.content[previous.len()..]);
        } else if logs.content.len() < previous.len() {
            print!("\n[enclave] log stream reset; showing current log content\n");
            print!("{}", logs.content);
        } else {
            let common_prefix = common_prefix_byte_len(&previous, &logs.content);
            if common_prefix == 0 {
                print!("\n[enclave] log stream changed; showing current log content\n");
            }
            print!("{}", &logs.content[common_prefix..]);
        }
        std::io::stdout().flush()?;
        previous = logs.content;
    }
}

fn common_prefix_byte_len(left: &str, right: &str) -> usize {
    let mut len = 0usize;
    for ((left_offset, left_char), (_, right_char)) in left.char_indices().zip(right.char_indices())
    {
        if left_char != right_char {
            break;
        }
        len = left_offset + left_char.len_utf8();
    }
    len
}

fn fetch_workspace_logs(
    ctx: &WorkspaceCommandContext<'_>,
    sandbox: &str,
    workspace: &str,
    tail: Option<usize>,
) -> Result<WorkspaceLogsResult> {
    let response = send_managed(
        ctx.socket,
        "workspace.logs",
        json!({
            "sandbox": sandbox,
            "workspace": workspace,
            "tail": tail,
        }),
    )?;
    serde_json::from_value(response).map_err(Into::into)
}

fn run_workspace_stats(
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

fn run_workspace_snapshot(
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

fn run_workspace_snapshot_list(
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

fn run_workspace_restore(
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

fn run_workspace_snapshot_gc(
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

fn run_workspace_exec(ctx: &WorkspaceCommandContext<'_>, args: WorkspaceExecArgs) -> Result<()> {
    enter::run_workspace_exec_direct(ctx, args)
}

fn resolve_workspace_target_from_optional(
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
