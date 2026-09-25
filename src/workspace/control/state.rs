use super::*;
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
    mark_workspace_stopped(sandbox, workspace_id)?;
    remove_sandbox_cgroup_if_idle(sandbox);
    // Verify the host state the cleanup claimed to release. A registry mutation
    // succeeding is not evidence that the runtime, its cgroup, its mounts, or its
    // loop device are gone, so the certificate is what makes the stop trustworthy.
    // It runs after the runtime markers are removed because those markers are one
    // of the things it verifies.
    let certificate = crate::workspace::verify_workspace_cleanup(&workspace, network.as_ref());
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
fn remove_workspace_runtime_markers(workspace: &WorkspaceMetadata) {
    let pid_file = session::runtime_pid_file(workspace);
    if let Err(err) = fs::remove_file(&pid_file) {
        if err.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!("failed to remove {}: {err:#}", pid_file.display());
        }
    }
    let ready_file = session::runtime_ready_file(workspace);
    if let Err(err) = fs::remove_file(&ready_file) {
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

pub(crate) fn clear_workspace_namespace_refs(workspace: &mut WorkspaceMetadata) {
    if let Err(err) = session::clear_namespace_ref_files(workspace) {
        tracing::warn!(
            "failed to clear namespace refs for workspace {}: {err:#}",
            workspace.id
        );
    }
    workspace.namespace_refs = Default::default();
}

pub(crate) fn remove_sandbox_cgroup_if_idle(sandbox: &RegistrySandbox) {
    if sandbox
        .workspaces
        .values()
        .all(|item| !item.status.may_have_runtime())
        && !sandbox.metadata.limits.has_limits()
    {
        remove_sandbox_cgroup(&sandbox.metadata);
    }
}

pub(crate) fn remove_sandbox_cgroup(sandbox: &SandboxMetadata) {
    let sandbox_path = std::path::PathBuf::from("/sys/fs/cgroup")
        .join(crate::sandbox::cgroup::sandbox_cgroup_name(&sandbox.id));
    if let Err(err) = crate::sandbox::cgroup::remove_cgroup_path(&sandbox_path) {
        tracing::debug!(
            "sandbox cgroup cleanup skipped for '{}': {err:#}",
            sandbox.id
        );
    }
}

pub(crate) fn resolve_workspace_id(sandbox: &RegistrySandbox, selector: &str) -> Result<String> {
    if sandbox.workspaces.contains_key(selector) {
        return Ok(selector.to_string());
    }

    let mut matches = Vec::new();
    for (id, workspace) in &sandbox.workspaces {
        if workspace.name == selector {
            matches.push(id.clone());
        }
    }

    match matches.len() {
        0 => Err(crate::error::coded(
            crate::error::ErrorCode::NotFound,
            format!(
                "workspace '{}' not found in sandbox '{}'",
                selector, sandbox.metadata.id
            ),
        )),
        1 => Ok(matches.remove(0)),
        _ => Err(crate::error::coded(
            crate::error::ErrorCode::Conflict,
            format!(
                "workspace name '{}' is ambiguous in sandbox '{}'; use id instead (matches: {})",
                selector,
                sandbox.metadata.id,
                matches.join(", ")
            ),
        )),
    }
}

/// Release the host resources a workspace still owns after its runtime is gone.
///
/// A runtime that died without a clean stop leaves its cgroup, its veth and
/// anti-spoofing rules, its storage mounts, and its private tmp behind. The
/// registry record is the only description of what to release, so the teardown
/// runs before the record is cleared. A failed teardown keeps the record instead
/// of claiming a clean stop while resources are still held.
fn release_orphaned_workspace_resources(
    sandbox: &SandboxMetadata,
    workspace: &WorkspaceMetadata,
    reason: &str,
) -> bool {
    let mut workspace = workspace.clone();
    // An address outside the Enclave subnet cannot own an Enclave interface, so
    // there is nothing to release for it. Treating it as a release failure would
    // keep the record for a resource that does not exist and block the recovery
    // of everything else the workspace still holds.
    if let Some(ip) = workspace.assigned_ip.clone() {
        if crate::network::ipam::parse_host_octet(&ip).is_none() {
            tracing::warn!(
                "reconcile: {reason} workspace '{}' recorded the non-Enclave address {ip}; skipping its network release",
                workspace.id
            );
            workspace.assigned_ip = None;
        }
    }
    if let Err(error) = cleanup::run_workspace_stop_cleanup(
        WorkspaceStopCleanup {
            sandbox: sandbox.clone(),
            workspace: workspace.clone(),
        },
        false,
        false,
    ) {
        tracing::warn!(
            "reconcile: cannot release the resources of {reason} workspace '{}': {error:#}",
            workspace.id
        );
        return false;
    }
    true
}

pub(crate) fn reconcile_workspace_runtime_state(
    sandbox: &SandboxMetadata,
    workspace: &mut WorkspaceMetadata,
) -> Result<bool> {
    // An interrupted transition is resolved deterministically by rolling it
    // back: a launch that never committed is not resumed, and a stop that never
    // committed is completed. Never resume a half-started runtime, because its
    // recorded identity may not match the process that is actually running.
    if workspace.status.is_transitional() {
        let interrupted = workspace.status.clone();
        if let Some((pid, starttime)) = workspace.runtime_pid.zip(workspace.runtime_starttime_ticks)
        {
            if session::process_matches(pid, Some(starttime)) {
                // A live runtime from the interrupted operation must be stopped
                // before the workspace can be reported as cleanly stopped.
                if let Err(error) = session::stop_session(pid, Some(starttime)) {
                    tracing::warn!(
                        "reconcile: failed to stop runtime {} for interrupted {:?} workspace '{}': {error:#}",
                        pid,
                        interrupted,
                        workspace.id
                    );
                    return Ok(false);
                }
            }
        }
        // Tear down everything the interrupted operation may have created —
        // workspace cgroup, network, storage mounts, `/tmp` — so the rollback
        // leaves nothing behind and the next start begins clean. A failed
        // teardown keeps the record transitional instead of claiming the
        // workspace is stopped while resources are still held.
        if let Err(error) = cleanup::run_workspace_stop_cleanup(
            WorkspaceStopCleanup {
                sandbox: sandbox.clone(),
                workspace: workspace.clone(),
            },
            false,
            false,
        ) {
            tracing::warn!(
                "reconcile: cleanup after interrupted {:?} transition for workspace '{}' is incomplete: {error:#}",
                interrupted,
                workspace.id
            );
            return Ok(false);
        }
        tracing::warn!(
            "reconcile: rolled back interrupted {:?} transition for workspace '{}'",
            interrupted,
            workspace.id
        );
        workspace.status = WorkspaceStatus::Stopped;
        workspace.runtime_pid = None;
        workspace.runtime_starttime_ticks = None;
        workspace.assigned_ip = None;
        clear_workspace_namespace_refs(workspace);
        remove_workspace_runtime_markers(workspace);
        persist_workspace_metadata(workspace)?;
        return Ok(true);
    }

    if !workspace.status.is_running() {
        // A stopped workspace keeps no runtime identity. A recorded PID or an
        // assigned IP is a leftover from a stop that did not finish, and either
        // one means the host may still hold the workspace's cgroup, interface,
        // firewall rules, or mounts. Namespace reference files count too, but an
        // empty reference is the normal post-stop value rather than a leftover.
        let holds_host_resources =
            workspace.runtime_pid.is_some() || workspace.assigned_ip.is_some();
        let namespace_refs_recorded = |value: &str| !value.is_empty() && value != "unassigned";
        let stale_runtime_state = holds_host_resources
            || namespace_refs_recorded(&workspace.namespace_refs.mount)
            || namespace_refs_recorded(&workspace.namespace_refs.pid)
            || session::namespace_ref_files_exist(workspace);
        if !stale_runtime_state {
            return Ok(false);
        }
        if holds_host_resources
            && !release_orphaned_workspace_resources(sandbox, workspace, "stopped")
        {
            return Ok(false);
        }
        workspace.runtime_pid = None;
        workspace.runtime_starttime_ticks = None;
        workspace.assigned_ip = None;
        clear_workspace_namespace_refs(workspace);
        persist_workspace_metadata(workspace)?;
        return Ok(true);
    }

    let runtime_is_live = workspace
        .runtime_pid
        .zip(workspace.runtime_starttime_ticks)
        .is_some_and(|(pid, starttime)| session::process_matches(pid, Some(starttime)));
    if !runtime_is_live {
        // The record says the workspace is running but its runtime is gone, so
        // everything the runtime owned is now unowned. Release it before the
        // record is cleared: the record is the only description of what to
        // release, and clearing it first would leave the interface, its firewall
        // rules, and its mounts on the host with nothing left to find them by.
        if !release_orphaned_workspace_resources(sandbox, workspace, "dead-runtime") {
            return Ok(false);
        }
        workspace.status = WorkspaceStatus::Stopped;
        workspace.runtime_pid = None;
        workspace.runtime_starttime_ticks = None;
        workspace.assigned_ip = None;
        clear_workspace_namespace_refs(workspace);
        remove_workspace_runtime_markers(workspace);
        persist_workspace_metadata(workspace)?;
        return Ok(true);
    }

    let pid = workspace.runtime_pid.expect("live runtime has a pid");
    if session::namespace_refs_match_runtime(workspace, pid) {
        return Ok(false);
    }

    let (mount_ns, pid_ns) = session::read_namespace_refs(pid)?;
    normalize_namespace_ref_paths(workspace);
    session::write_namespace_ref_values(workspace, &mount_ns, &pid_ns)?;
    persist_workspace_metadata(workspace)?;
    Ok(true)
}
