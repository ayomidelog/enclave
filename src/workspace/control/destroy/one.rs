//! Releasing one workspace and removing its record.

use super::super::*;
use super::*;

/// How many times a destroy re-resolves a workspace whose runtime appeared
/// while it was being cleaned.
///
/// The bound only has to cover a stream of competing starts. Each pass stops
/// whatever runtime is now recorded, so one is enough for the ordinary race and
/// the rest are for a caller starting the same workspace in a loop.
const DESTROY_ATTEMPTS: usize = 8;

/// A workspace that has been released and removed, and the evidence that it was.
pub(super) struct DestroyedWorkspace {
    pub(super) workspace: WorkspaceMetadata,
    pub(super) outcome: cleanup::CleanupOutcome,
    pub(super) certificate: crate::workspace::WorkspaceCleanupCertificate,
}

/// The sandbox and workspace as the registry describes them right now.
pub(super) fn current_workspace(
    state_dir: &std::path::Path,
    sandbox_id: &str,
    workspace_id: &str,
) -> Result<Option<(SandboxMetadata, WorkspaceMetadata)>> {
    with_registry(state_dir, |registry| {
        let Some(sandbox) = registry.sandboxes.get(sandbox_id) else {
            return Ok(None);
        };
        let Some(workspace) = sandbox.workspaces.get(workspace_id) else {
            return Ok(None);
        };
        Ok(Some((sandbox.metadata.clone(), workspace.clone())))
    })
}

/// Release one workspace and remove its record, from the record as it stands now.
///
/// The record is read again here rather than taken from the caller's plan, and the
/// removal re-checks under the registry lock that it still describes the runtime the
/// cleanup stopped. A start that commits in between creates a runtime this destroy
/// never saw, and removing the record then would delete the only description of a live
/// process: the session would keep running with its cgroup, its interface, and its
/// mounts, and nothing left on the host would name them. When that happens the
/// workspace is resolved again and the pass repeats, so the runtime is stopped rather
/// than orphaned. A start that has not committed yet is safe either way, because its
/// commit requires the record it reserved and rolls back when the record is gone.
///
/// Returns `None` when the workspace is already gone.
pub(super) fn destroy_one(
    state_dir: &std::path::Path,
    sandbox_id: &str,
    workspace_id: &str,
    mode: cleanup::CleanupMode,
    mut journal: Option<&mut crate::operation::Journal>,
) -> Result<Option<DestroyedWorkspace>> {
    for _ in 0..DESTROY_ATTEMPTS {
        let Some((sandbox, workspace)) = current_workspace(state_dir, sandbox_id, workspace_id)?
        else {
            return Ok(None);
        };
        if let Some(journal) = journal.as_deref_mut() {
            journal.phase("cleanup")?;
        }
        // Take the inventory before anything is released: it is the list of what this
        // workspace owned, and it is what the certificate re-checks afterwards to
        // prove the diff is empty rather than only that the calls succeeded.
        let inventory = crate::workspace::ResourceInventory::collect(&sandbox.id, &workspace);
        // The caller owns the journal and fails it, so the record's outcome is
        // written in one place rather than here and there.
        let outcome = cleanup::cleanup_workspace_artifacts(&sandbox, &workspace, mode)?;
        if !outcome.retained.is_empty() {
            // Force mode removes the registry record and leaves the host state to
            // `doctor --repair`, so the journal has to name what was left behind.
            if let Some(journal) = journal.as_deref_mut() {
                journal.phase("retain_unreleased_resources")?;
            }
            tracing::warn!(
                "workspace '{}' destroy left resources behind: {}",
                workspace.id,
                outcome.retained_summary()
            );
        }

        // Verify the host after deletion rather than inferring it from the removal
        // calls: a directory removal succeeding is not evidence that the loop device
        // was detached or that the mounts are gone. In normal mode the artifact
        // cleanup already refused to delete anything it could not release, so this
        // catches what only becomes visible once the files are gone. It runs before
        // the registry record is removed, so a failure keeps the evidence.
        let network_complete = !outcome
            .retained
            .iter()
            .any(|retained| retained.resource == "network");
        let certificate =
            crate::workspace::verify_workspace_destroyed(&workspace, network_complete)
                .with_inventory(inventory);
        if !mode.is_force() && !certificate.is_complete() {
            return Err(crate::error::coded(
                crate::error::ErrorCode::CleanupIncomplete,
                format!(
                    "workspace '{}' destroy left resources behind, retaining its registry record: {}",
                    workspace.id,
                    certificate.failure_summary()
                ),
            ));
        }
        if let Some(journal) = journal.as_deref_mut() {
            journal.phase("remove_registry_record")?;
        }

        let removed = with_registry_mut(state_dir, |registry| {
            let Some(sandbox) = registry.sandboxes.get_mut(sandbox_id) else {
                return Ok(true);
            };
            match sandbox.workspaces.get(workspace_id) {
                // Gone already, which is what this pass wanted.
                None => Ok(true),
                Some(current)
                    if current.runtime_pid == workspace.runtime_pid
                        && current.runtime_starttime_ticks == workspace.runtime_starttime_ticks =>
                {
                    sandbox.workspaces.remove(workspace_id);
                    Ok(true)
                }
                // A runtime this pass did not stop is recorded now, so the record is
                // the only thing that describes it and it stays.
                Some(_) => Ok(false),
            }
        })?;
        if removed {
            // The files were removed before the record was, and removing the record is the
            // last thing the registry lock is held for. An operation that had already
            // resolved this workspace before the destroy began can persist its own metadata
            // while the destroy is releasing host state, which recreates the directory after
            // the removal above and before the record goes. The record is gone now, so
            // nothing can resolve the workspace again and no further write can arrive: the
            // directory is removed once more, so the files are gone at the moment the destroy
            // returns rather than only at the moment the record was still there. Force mode
            // keeps the files whenever anything is still held, because the directory is the
            // only remaining description of what is running, so the removal is conditional
            // on the artifact cleanup having removed them.
            if outcome.files_removed {
                cleanup::remove_workspace_directory_after_record_removal(&sandbox, &workspace)?;
            }
            return Ok(Some(DestroyedWorkspace {
                workspace,
                outcome,
                certificate,
            }));
        }
        tracing::warn!(
            "workspace '{}' gained a runtime while it was being destroyed; resolving it again",
            workspace_id
        );
    }
    bail!(
        "workspace '{}' kept gaining a runtime while it was being destroyed",
        workspace_id
    )
}

pub fn remove_workspace(
    state_dir: &std::path::Path,
    sandbox_selector: &str,
    workspace_selector: &str,
) -> Result<()> {
    destroy_workspace(state_dir, sandbox_selector, workspace_selector).map(|_| ())
}

pub fn destroy_workspace(
    state_dir: &std::path::Path,
    sandbox_selector: &str,
    workspace_selector: &str,
) -> Result<String> {
    destroy_workspace_with_mode(
        state_dir,
        sandbox_selector,
        workspace_selector,
        CleanupMode::Normal,
    )
    .map(|report| report.workspace_id)
}

/// Destroy a workspace and report what was released and what was retained.
pub fn destroy_workspace_with_mode(
    state_dir: &std::path::Path,
    sandbox_selector: &str,
    workspace_selector: &str,
    mode: CleanupMode,
) -> Result<WorkspaceDestroyReport> {
    let (sandbox, workspace) = with_registry(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, sandbox_selector)?;
        let sandbox = registry
            .sandboxes
            .get(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_id))?;

        let workspace_id = resolve_workspace_id(sandbox, workspace_selector)?;
        let workspace = sandbox
            .workspaces
            .get(&workspace_id)
            .cloned()
            .ok_or_else(|| {
                anyhow!(
                    "workspace '{}' not found in sandbox '{}'",
                    workspace_id,
                    sandbox_id
                )
            })?;

        Ok((sandbox.metadata.clone(), workspace))
    })?;

    let workspace_id = workspace.id.clone();
    let mut journal = crate::operation::Journal::begin(
        state_dir,
        "workspace.destroy",
        format!("{}/{}", sandbox.id, workspace_id),
    )?;
    let destroyed = match destroy_one(
        state_dir,
        &sandbox.id,
        &workspace_id,
        mode,
        Some(&mut journal),
    ) {
        Ok(Some(destroyed)) => destroyed,
        Ok(None) => {
            journal.succeed()?;
            return Ok(WorkspaceDestroyReport {
                workspace_id,
                mode,
                retained: Vec::new(),
                certificate: Default::default(),
            });
        }
        Err(error) => {
            let _ = journal.fail(format!("{error:#}"));
            return Err(error);
        }
    };
    journal.succeed()?;

    Ok(WorkspaceDestroyReport {
        workspace_id,
        mode,
        retained: destroyed.outcome.retained,
        certificate: destroyed.certificate,
    })
}
