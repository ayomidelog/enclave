//! Printing workspace state for the CLI.
//!
//! The printers are grouped by what they show: the workspace list and one
//! workspace's status, the live stats table, and the snapshot list. The small
//! value formatters they share live in one place so a size or a percentage reads
//! the same everywhere.

use crate::workspace::{
    PublishedPortState, PublishedPortStatus, WorkspaceListItem, WorkspaceMetadata,
    WorkspaceSnapshotInfo, WorkspaceStatsReport, WorkspaceStatus, WorkspaceStatusReport,
};

mod format;
mod snapshots;
mod stats;
mod workspaces;

pub(in crate::commands::workspace) use snapshots::print_snapshot_list;
pub(in crate::commands::workspace) use stats::print_workspace_stats;
pub(crate) use stats::print_workspace_stats_table;
pub(in crate::commands::workspace) use workspaces::{
    print_workspace_list_all, print_workspace_list_by_sandbox, print_workspace_ports,
    print_workspace_status,
};

#[cfg(test)]
#[path = "../../../../tests/src/commands/workspace/display.rs"]
mod tests;
