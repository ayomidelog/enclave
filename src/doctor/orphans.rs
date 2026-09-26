use std::fs;
use std::path::{Path, PathBuf};

use crate::registry::with_registry;

use super::DoctorCheck;

/// Report a workspace directory that a live runtime still owns but that nothing
/// in the registry describes.
///
/// `workspace.json` is the only file that records a runtime's pid. When it is
/// gone the registry has no record to reconcile against, and repair retains the
/// directory rather than deleting a running workspace's files. Retaining is the
/// safe half; this check is the other half, because a retained directory with a
/// live runtime is a leak the operator has to be told about.
///
/// Discovery reads only the markers the runtime wrote, so a reused pid is never
/// signalled and a foreign process is never named.
pub(crate) fn check_orphan_runtimes(state_dir: &Path) -> DoctorCheck {
    const NAME: &str = "orphan_runtimes";

    let sandboxes = match with_registry(state_dir, |registry| {
        Ok(registry
            .sandboxes
            .values()
            .map(|sandbox| {
                (
                    sandbox.metadata.id.clone(),
                    PathBuf::from(&sandbox.metadata.workspaces_path),
                    sandbox
                        .workspaces
                        .keys()
                        .cloned()
                        .collect::<std::collections::BTreeSet<_>>(),
                )
            })
            .collect::<Vec<_>>())
    }) {
        Ok(sandboxes) => sandboxes,
        Err(error) => {
            return DoctorCheck::warn(
                NAME,
                &format!("failed to read workspace ownership: {error:#}"),
            )
        }
    };

    let mut found = Vec::new();
    for (sandbox_id, workspaces_path, known) in &sandboxes {
        let Ok(entries) = fs::read_dir(workspaces_path) else {
            continue;
        };
        for entry in entries.flatten() {
            let workspace_dir = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_dir() {
                continue;
            }
            let workspace_id = entry.file_name().to_string_lossy().to_string();
            // A workspace the registry knows about is accounted for by the other
            // checks; this one is specifically about what the registry lost.
            if known.contains(&workspace_id) {
                continue;
            }
            if let Some(orphan) =
                crate::workspace::find_orphan_runtime(&workspace_dir, sandbox_id, &workspace_id)
            {
                found.push(format!(
                    "{sandbox_id}/{workspace_id} at {}: {}",
                    workspace_dir.display(),
                    orphan.describe()
                ));
            }
        }
    }

    if found.is_empty() {
        return DoctorCheck::ok(
            NAME,
            "every workspace directory is either in the registry or has no live runtime",
        );
    }
    DoctorCheck::warn(
        NAME,
        &format!(
            "{} workspace directory(ies) are owned by a live runtime the registry does not describe; stop the runtime, then run `enclave doctor --repair`: {}",
            found.len(),
            found.join("; ")
        ),
    )
}

#[cfg(test)]
#[path = "../../tests/src/doctor/orphans.rs"]
mod tests;
