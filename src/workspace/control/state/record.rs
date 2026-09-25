//! Writing a workspace's registry record and its on-disk metadata.

use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};

use crate::registry::RegistrySandbox;
use crate::workspace::cleanup::{self, WorkspaceStopCleanup};
use crate::workspace::session;
use crate::workspace::types::{WorkspaceMetadata, WorkspaceStatus};
use crate::workspace::WorkspaceCleanupCertificate;

pub(crate) fn set_workspace_stopped(
    sandbox: &mut RegistrySandbox,
    workspace_id: &str,
) -> Result<WorkspaceCleanupCertificate> {
    let workspace = sandbox
        .workspaces
        .get(workspace_id)
        .cloned()
        .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_id))?;
    let network = cleanup::run_workspace_stop_cleanup(
        WorkspaceStopCleanup {
            sandbox: sandbox.metadata.clone(),
            workspace: workspace.clone(),
        },
        false,
        false,
    )?;
    commit_workspace_stopped(sandbox, workspace_id, network.as_ref())
}

/// Record a workspace as stopped once its host resources have been released.
///
/// The host work is deliberately not part of this: unmounting storage and
/// tearing down a network is tens of milliseconds of work that neither reads nor
/// writes the registry, and the caller runs it before taking the registry lock
/// so one workspace's teardown does not stall every other lifecycle request.
/// This half is registry-only apart from the verification, which is what makes
/// the recorded state trustworthy rather than merely written.
pub(crate) fn commit_workspace_stopped(
    sandbox: &mut RegistrySandbox,
    workspace_id: &str,
    network: Option<&crate::network::NetworkCleanupReport>,
) -> Result<WorkspaceCleanupCertificate> {
    // The record as it was before the stop, which is what verification has to
    // answer for: it names the runtime, the address, and the storage this
    // workspace owned.
    let workspace = sandbox
        .workspaces
        .get(workspace_id)
        .cloned()
        .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_id))?;
    mark_workspace_stopped(sandbox, workspace_id)?;
    super::cgroups::remove_sandbox_cgroup_if_idle(sandbox);
    // Verify the host state the cleanup claimed to release. A registry mutation
    // succeeding is not evidence that the runtime, its cgroup, its mounts, or its
    // loop device are gone, so the certificate is what makes the stop trustworthy.
    // It runs after the runtime markers are removed because those markers are one
    // of the things it verifies.
    let certificate = crate::workspace::verify_workspace_cleanup(&workspace, network);
    if !certificate.is_complete() {
        bail!(
            "workspace '{}' cleanup is incomplete: {}",
            workspace_id,
            certificate.failure_summary()
        );
    }
    Ok(certificate)
}

pub(crate) fn mark_workspace_stopped(
    sandbox: &mut RegistrySandbox,
    workspace_id: &str,
) -> Result<()> {
    let workspace = sandbox
        .workspaces
        .get_mut(workspace_id)
        .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_id))?;
    workspace.status = WorkspaceStatus::Stopped;
    workspace.runtime_pid = None;
    workspace.runtime_starttime_ticks = None;
    workspace.assigned_ip = None;
    clear_workspace_namespace_refs(workspace);
    remove_workspace_runtime_markers(workspace);
    let metadata_path = PathBuf::from(&workspace.workspace_path).join("workspace.json");
    let metadata_raw = serde_json::to_string_pretty(workspace)?;
    crate::fsutil::write_file_atomic(&metadata_path, metadata_raw.as_bytes(), 0o600).with_context(
        || {
            format!(
                "failed to persist workspace state to {}",
                metadata_path.display()
            )
        },
    )?;
    Ok(())
}

/// Remove the on-disk runtime markers for a workspace that is no longer running.
pub(super) fn remove_workspace_runtime_markers(workspace: &WorkspaceMetadata) {
    let pid_file = session::runtime_pid_file(workspace);
    if let Err(err) = std::fs::remove_file(&pid_file) {
        if err.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!("failed to remove {}: {err:#}", pid_file.display());
        }
    }
    let ready_file = session::runtime_ready_file(workspace);
    if let Err(err) = std::fs::remove_file(&ready_file) {
        if err.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!("failed to remove {}: {err:#}", ready_file.display());
        }
    }
}

pub(crate) fn normalize_namespace_ref_paths(workspace: &mut WorkspaceMetadata) {
    let (mount_ref_path, pid_ref_path) = session::namespace_ref_paths(workspace);
    workspace.namespace_refs.mount = mount_ref_path.to_string_lossy().to_string();
    workspace.namespace_refs.pid = pid_ref_path.to_string_lossy().to_string();
}

pub(crate) fn persist_workspace_metadata(workspace: &WorkspaceMetadata) -> Result<()> {
    let metadata_path = PathBuf::from(&workspace.workspace_path).join("workspace.json");
    let metadata_raw = serde_json::to_string_pretty(workspace)?;
    crate::fsutil::write_file_atomic(&metadata_path, metadata_raw.as_bytes(), 0o600).with_context(
        || {
            format!(
                "failed to persist workspace metadata to {}",
                metadata_path.display()
            )
        },
    )
}

pub(super) fn clear_workspace_namespace_refs(workspace: &mut WorkspaceMetadata) {
    if let Err(err) = session::clear_namespace_ref_files(workspace) {
        tracing::warn!(
            "failed to clear namespace refs for workspace {}: {err:#}",
            workspace.id
        );
    }
    workspace.namespace_refs = Default::default();
}
