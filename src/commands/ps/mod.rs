mod display;

use std::collections::HashSet;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde_json::json;

use crate::cli::PsArgs;
use crate::workspace::ProcessEntry;

use super::send_managed;

pub(crate) fn run_ps(socket: &Path, args: PsArgs) -> Result<()> {
    let response = send_managed(socket, "process.list", json!({}))?;
    let mut entries: Vec<ProcessEntry> = serde_json::from_value(response)?;
    if args.local {
        entries = filter_local_project_entries(entries)?;
    }
    display::print_process_table(&entries);
    Ok(())
}

fn filter_local_project_entries(entries: Vec<ProcessEntry>) -> Result<Vec<ProcessEntry>> {
    let cwd = std::env::current_dir().context("failed to determine current directory")?;
    let enclavefile_path = crate::enclavefile::find_enclavefile(&cwd).ok_or_else(|| {
        anyhow::anyhow!(
            "no Enclavefile found in {}. `enclave ps --local` must be run from a project directory.",
            cwd.display()
        )
    })?;
    let enclavefile = crate::enclavefile::load_enclavefile(&enclavefile_path)?;
    let workspace_names: HashSet<&str> = enclavefile
        .workspace
        .values()
        .map(|ws| ws.name.as_str())
        .collect();
    if workspace_names.is_empty() {
        bail!(
            "Enclavefile at {} does not define any [workspace.*] entries",
            enclavefile_path.display()
        );
    }

    Ok(filter_entries_for_project(
        entries,
        &enclavefile.sandbox.name,
        &workspace_names,
    ))
}

fn filter_entries_for_project(
    entries: Vec<ProcessEntry>,
    sandbox_name: &str,
    workspace_names: &HashSet<&str>,
) -> Vec<ProcessEntry> {
    entries
        .into_iter()
        .filter(|entry| {
            entry.sandbox == sandbox_name && workspace_names.contains(entry.workspace.as_str())
        })
        .collect()
}
