use std::path::Path;

use anyhow::Result;
use serde_json::json;

use crate::cli::{RegistryCommands, RegistryRepairArgs};
use crate::registry::RepairReport;

use super::{print_operation_id, send_managed};

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
        "repair complete (added_sandboxes={}, removed_sandboxes={}, added_workspaces={}, removed_workspaces={}, reconciled_runtime_records={})",
        report.added_sandboxes,
        report.removed_sandboxes,
        report.added_workspaces,
        report.removed_workspaces,
        report.reconciled_runtime_records
    );
    // A retained directory is a leak the operator has to act on, so it is printed
    // rather than left in the daemon log. The fix is to stop the runtime the
    // directory still belongs to; repair cannot do it, because signalling a
    // process it did not start is exactly what it must not do.
    if !report.retained_orphans.is_empty() {
        eprintln!(
            "{} workspace directory(ies) were retained because a live runtime still owns them:",
            report.retained_orphans.len()
        );
        for orphan in &report.retained_orphans {
            eprintln!("  - {}", orphan.describe());
        }
        eprintln!("stop each runtime, then run this command again to finish the cleanup");
    }
    // A disagreement between the registry and the per-directory metadata means the
    // two copies of a workspace's state had diverged, which is what an interrupted
    // lifecycle step leaves behind. Repair resolved it by adopting the on-disk
    // copy, so this is a record of what changed rather than a warning: it is
    // printed so an operator who saw a workspace in an unexpected state can see
    // that repair was the reason.
    if !report.metadata_disagreements.is_empty() {
        println!(
            "{} record(s) had registry and on-disk metadata that disagreed; the on-disk copy was adopted:",
            report.metadata_disagreements.len()
        );
        for disagreement in &report.metadata_disagreements {
            let target = match disagreement.workspace_id.as_deref() {
                Some(workspace) => format!("{}/{}", disagreement.sandbox_id, workspace),
                None => disagreement.sandbox_id.clone(),
            };
            println!("  - {}: {}", target, disagreement.differences.join(", "));
        }
    }
    print_operation_id();
    Ok(())
}
