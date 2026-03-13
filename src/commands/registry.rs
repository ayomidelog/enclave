use std::path::Path;

use anyhow::Result;
use serde_json::json;

use crate::cli::{RegistryCommands, RegistryRepairArgs};
use crate::registry::RepairReport;

use super::send_managed;

pub(crate) fn run_registry_command(socket: &Path, command: RegistryCommands) -> Result<()> {
    match command {
        RegistryCommands::Repair(args) => run_registry_repair(socket, args),
    }
}

fn run_registry_repair(socket: &Path, args: RegistryRepairArgs) -> Result<()> {
    tracing::info!("repairing registry...");
    let response = send_managed(
        socket,
        "registry.repair",
        json!({
            "strict": args.strict,
        }),
    )?;
    let report: RepairReport = serde_json::from_value(response)?;
    println!(
        "repair complete (added_sandboxes={}, removed_sandboxes={}, added_workspaces={}, removed_workspaces={})",
        report.added_sandboxes,
        report.removed_sandboxes,
        report.added_workspaces,
        report.removed_workspaces
    );
    Ok(())
}
