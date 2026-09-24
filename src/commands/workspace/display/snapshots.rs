//! The workspace snapshot list.

use super::format::column_width;
use anyhow::Result;

use super::*;

pub(in crate::commands::workspace) fn print_snapshot_list(
    snapshots: Vec<WorkspaceSnapshotInfo>,
) -> Result<()> {
    if snapshots.is_empty() {
        println!("no snapshots");
        return Ok(());
    }

    let name_width = column_width(&snapshots, |s| s.name.len(), "NAME".len());
    let created_width = column_width(&snapshots, |s| s.created_at.len(), "CREATED_AT".len());
    println!(
        "{:<name_width$} {:<created_width$} PATH",
        "NAME", "CREATED_AT"
    );
    for snapshot in snapshots {
        println!(
            "{:<name_width$} {:<created_width$} {}",
            snapshot.name, snapshot.created_at, snapshot.path
        );
    }
    Ok(())
}
