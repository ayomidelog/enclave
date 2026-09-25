//! Registry repair: reconcile `registry.json` with what is on disk.
//!
//! The registry is a cache of the sandboxes tree, so repair rebuilds it from the
//! tree rather than trusting either side alone. Reading the tree is [`scan`],
//! removing a directory nothing owns is [`orphan`], and this module turns the two
//! into one reconciled record.

mod orphan;
mod scan;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::workspace::OrphanRuntime;

use super::storage::{
    load_registry_with_migrations, registry_lock_path, save_registry_unlocked, update_cache,
};
use super::{ensure_registry, Registry, RegistrySandbox, RepairReport};

use orphan::remove_stale_creation_staging;
use scan::scan_on_disk;

/// A workspace directory repair refused to remove because a live runtime owns it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RetainedOrphan {
    pub sandbox_id: String,
    pub workspace_id: String,
    pub workspace_dir: PathBuf,
    pub runtime: OrphanRuntime,
}

impl RetainedOrphan {
    /// One line naming the directory and what still owns it.
    pub fn describe(&self) -> String {
        format!(
            "{}/{} at {} ({})",
            self.sandbox_id,
            self.workspace_id,
            self.workspace_dir.display(),
            self.runtime.describe()
        )
    }
}

/// What one scan of the sandboxes tree found.
#[derive(Default)]
pub(super) struct DiskScan {
    pub(super) sandboxes: BTreeMap<String, RegistrySandbox>,
    /// Workspace directories retained because a live runtime still owns them.
    pub(super) retained_orphans: Vec<RetainedOrphan>,
}

pub fn repair_registry(state_dir: &Path, strict: bool) -> Result<RepairReport> {
    ensure_registry(state_dir)?;
    let lock_path = registry_lock_path(state_dir);
    crate::fsutil::with_file_lock(&lock_path, || {
        let (mut registry, migrations) = match load_registry_with_migrations(state_dir) {
            Ok(loaded) => loaded,
            Err(err) => {
                tracing::warn!(
                    "registry repair is rebuilding in-memory state after registry load failure: {err:#}"
                );
                (Registry::default(), Vec::new())
            }
        };
        let mut report = RepairReport {
            // The version the record was written with, so an operator can see
            // that repair understood an older schema rather than overwriting it.
            migrated_registry_from_version: migrations.first().map(|step| step.from),
            ..RepairReport::default()
        };

        let sandboxes_root = state_dir.join("sandboxes");
        fs::create_dir_all(&sandboxes_root)
            .with_context(|| format!("failed to create {}", sandboxes_root.display()))?;

        let DiskScan {
            sandboxes: discovered,
            retained_orphans,
        } = scan_on_disk(state_dir, strict, &registry)?;
        report.retained_orphans = retained_orphans;

        // Garbage from creates that died, swept here because repair is the one
        // place that knows the staging tree is not part of the registry.
        if let Err(err) = remove_stale_creation_staging(state_dir) {
            tracing::warn!("failed to sweep the creation staging tree: {err:#}");
        }

        reconcile(&mut registry, &discovered, &mut report);

        save_registry_unlocked(state_dir, &registry)?;
        update_cache(state_dir, registry);
        Ok(report)
    })
}

/// Make the registry describe exactly what the scan found.
///
/// A sandbox on disk is kept and its metadata refreshed, a sandbox in the
/// registry but not on disk is dropped, and a workspace inside a discovered
/// sandbox is added or removed to match. The counters record each of those so the
/// report names what changed rather than only that something did.
fn reconcile(
    registry: &mut Registry,
    discovered: &BTreeMap<String, RegistrySandbox>,
    report: &mut RepairReport,
) {
    for (sandbox_id, discovered_sandbox) in discovered {
        match registry.sandboxes.get_mut(sandbox_id) {
            Some(existing) => {
                existing.metadata = discovered_sandbox.metadata.clone();

                for (workspace_id, workspace) in &discovered_sandbox.workspaces {
                    if !existing.workspaces.contains_key(workspace_id) {
                        report.added_workspaces += 1;
                    }
                    existing
                        .workspaces
                        .insert(workspace_id.clone(), workspace.clone());
                }

                let stale_ids: Vec<String> = existing
                    .workspaces
                    .keys()
                    .filter(|id| !discovered_sandbox.workspaces.contains_key(*id))
                    .cloned()
                    .collect();
                for workspace_id in stale_ids {
                    existing.workspaces.remove(&workspace_id);
                    report.removed_workspaces += 1;
                }
            }
            None => {
                report.added_sandboxes += 1;
                report.added_workspaces += discovered_sandbox.workspaces.len();
                registry
                    .sandboxes
                    .insert(sandbox_id.clone(), discovered_sandbox.clone());
            }
        }
    }

    let stale_sandbox_ids: Vec<String> = registry
        .sandboxes
        .keys()
        .filter(|id| !discovered.contains_key(*id))
        .cloned()
        .collect();
    for sandbox_id in stale_sandbox_ids {
        if let Some(removed) = registry.sandboxes.remove(&sandbox_id) {
            report.removed_sandboxes += 1;
            report.removed_workspaces += removed.workspaces.len();
        }
    }
}
