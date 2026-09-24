mod cp;
pub(crate) mod display;
mod enter;
mod exec;
mod lifecycle;
mod logs;
mod ports;
mod snapshots;

// Shared prelude for the submodules below: they each start with `use super::*`,
// so the argument types, result types, and request helpers they need are
// declared once here instead of being repeated in every file.
use std::io::Write;
use std::path::Path;
use std::thread;
use std::time::Duration;

use anyhow::{bail, Result};
use serde_json::json;

use crate::cli::{
    SnapshotCommands, WorkspaceCommands, WorkspaceCreateArgs, WorkspaceDestroyArgs,
    WorkspaceExecArgs, WorkspaceListArgs, WorkspaceLogsArgs, WorkspacePortCommands,
    WorkspacePortPublishArgs, WorkspacePortUnpublishArgs, WorkspaceRemoveArgs, WorkspaceResizeArgs,
    WorkspaceRestoreArgs, WorkspaceSnapshotArgs, WorkspaceSnapshotExportArgs,
    WorkspaceSnapshotGcArgs, WorkspaceSnapshotImportArgs, WorkspaceTargetArgs,
    WorkspaceTargetOrLocalArgs, WorkspaceWipeArgs,
};
use crate::workspace::{
    PublishedPortStatus, WorkspaceListItem, WorkspaceLogsResult, WorkspaceMetadata,
    WorkspaceSnapshotArchiveInfo, WorkspaceSnapshotInfo,
};

use super::{confirm_destructive_action, daemon, report_retained_resources, send, send_managed};

use exec::{resolve_workspace_target_from_optional, run_workspace_exec};
use lifecycle::{
    run_workspace_create, run_workspace_destroy, run_workspace_list, run_workspace_remove,
    run_workspace_resize, run_workspace_start, run_workspace_stats, run_workspace_status,
    run_workspace_stop, run_workspace_wipe,
};
use logs::run_workspace_logs;
use ports::run_workspace_port_command;
use snapshots::{
    run_workspace_restore, run_workspace_snapshot, run_workspace_snapshot_export,
    run_workspace_snapshot_gc, run_workspace_snapshot_import, run_workspace_snapshot_list,
};

struct WorkspaceCommandContext<'a> {
    socket: &'a Path,
}

const LOG_FOLLOW_POLL_INTERVAL: Duration = Duration::from_millis(500);
const LOG_FOLLOW_MAX_POLL_INTERVAL: Duration = Duration::from_secs(2);

pub(crate) fn run_workspace_command(socket: &Path, command: WorkspaceCommands) -> Result<()> {
    let ctx = WorkspaceCommandContext { socket };
    match command {
        WorkspaceCommands::Create(args) => run_workspace_create(&ctx, args),
        WorkspaceCommands::Resize(args) => run_workspace_resize(&ctx, args),
        WorkspaceCommands::Cp(args) => cp::run_workspace_cp(&ctx, args),
        WorkspaceCommands::List(args) => run_workspace_list(&ctx, args),
        WorkspaceCommands::Remove(args) => run_workspace_remove(&ctx, args),
        WorkspaceCommands::Wipe(args) => run_workspace_wipe(&ctx, args),
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
        SnapshotCommands::Export(args) => run_workspace_snapshot_export(&ctx, args),
        SnapshotCommands::Import(args) => run_workspace_snapshot_import(&ctx, args),
    }
}
