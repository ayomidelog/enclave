use std::path::Path;

use anyhow::Result;
use serde_json::json;

use crate::workspace::WorkspaceStatsReport;

use super::send_managed;

pub(crate) fn run_stats(socket: &Path) -> Result<()> {
    let response = send_managed(socket, "workspace.stats.list", json!({}))?;
    let stats: Vec<WorkspaceStatsReport> = serde_json::from_value(response)?;
    crate::commands::workspace::display::print_workspace_stats_table(&stats)
}
