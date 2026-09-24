use super::*;
use crate::workspace::WorkspaceCleanupCertificate;

pub fn stop_workspace(
    state_dir: &std::path::Path,
    sandbox_selector: &str,
    workspace_selector: &str,
) -> Result<WorkspaceMetadata> {
    stop_workspace_with_certificate(state_dir, sandbox_selector, workspace_selector)
        .map(|(metadata, _)| metadata)
}

/// Stop a workspace and return the verified cleanup certificate alongside it.
///
/// Callers that own host resources outside the workspace layer (the port
/// publisher, for example) can attach their own verification to the certificate
/// before reporting the stop as complete.
pub fn stop_workspace_with_certificate(
    state_dir: &std::path::Path,
    sandbox_selector: &str,
    workspace_selector: &str,
) -> Result<(WorkspaceMetadata, WorkspaceCleanupCertificate)> {
    let (sandbox_id, workspace_id, current) = with_registry(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, sandbox_selector)?;
        let sandbox = registry
            .sandboxes
            .get(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_id))?;
        let workspace_id = resolve_workspace_id(sandbox, workspace_selector)?;
        let current = sandbox
            .workspaces
            .get(&workspace_id)
            .cloned()
            .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_id))?;
        Ok((sandbox_id, workspace_id, current))
    })?;

    let mut journal = crate::operation::Journal::begin(
        state_dir,
        "workspace.stop",
        format!("{}/{}", sandbox_id, workspace_id),
    )?;
    journal.phase("stop_runtime")?;
    // Record the in-flight transition so an observer sees that teardown is
    // underway, and so a crash mid-stop leaves evidence instead of a workspace
    // that still claims to be running.
    mark_workspace_stopping(state_dir, &sandbox_id, &workspace_id)?;
    let stop_runtime = crate::perf::Timer::new("workspace.stop.runtime");
    if let Some(pid) = current.runtime_pid {
        if let Err(error) = session::stop_session(pid, current.runtime_starttime_ticks) {
            drop(stop_runtime);
            let _ = journal.fail(format!("{error:#}"));
            return Err(error);
        }
    }
    drop(stop_runtime);

    journal.phase("cleanup_resources")?;
    let cleanup_phase = crate::perf::Timer::new("workspace.stop.cleanup");
    let result = with_registry_mut(state_dir, |registry| {
        let sandbox = registry
            .sandboxes
            .get_mut(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_id))?;
        let latest = sandbox
            .workspaces
            .get(&workspace_id)
            .cloned()
            .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_id))?;
        if latest.runtime_pid != current.runtime_pid
            || latest.runtime_starttime_ticks != current.runtime_starttime_ticks
        {
            bail!(
                "workspace '{}' changed while it was stopping; refusing stale metadata commit",
                workspace_id
            );
        }
        let certificate = set_workspace_stopped(sandbox, &workspace_id)?;
        let result = sandbox
            .workspaces
            .get(&workspace_id)
            .cloned()
            .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_id))?;
        Ok((result, certificate))
    });
    drop(cleanup_phase);
    match result {
        Ok((metadata, certificate)) => {
            journal.succeed()?;
            Ok((metadata, certificate))
        }
        Err(error) => {
            let _ = journal.fail(format!("{error:#}"));
            Err(error)
        }
    }
}

/// Record that teardown of a workspace runtime has begun.
fn mark_workspace_stopping(
    state_dir: &std::path::Path,
    sandbox_id: &str,
    workspace_id: &str,
) -> Result<()> {
    with_registry_mut(state_dir, |registry| {
        let workspace = registry
            .sandboxes
            .get_mut(sandbox_id)
            .and_then(|sandbox| sandbox.workspaces.get_mut(workspace_id))
            .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_id))?;
        workspace.status = WorkspaceStatus::Stopping;
        persist_workspace_metadata(workspace)
    })
}

pub(crate) fn stop_running_workspaces_in_sandbox(
    state_dir: &std::path::Path,
    sandbox_selector: &str,
) -> Result<Vec<String>> {
    // Anything that may own a runtime is a stop target, including workspaces
    // left in a transitional state by an interrupted lifecycle operation.
    let targets = with_registry(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, sandbox_selector)?;
        let sandbox = registry
            .sandboxes
            .get(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_id))?;
        Ok(sandbox
            .workspaces
            .values()
            .filter(|workspace| workspace.status.may_have_runtime())
            .cloned()
            .collect::<Vec<_>>())
    })?;

    let stop_targets = targets
        .iter()
        .filter_map(|workspace| {
            workspace
                .runtime_pid
                .map(|pid| (pid, workspace.runtime_starttime_ticks))
        })
        .collect::<Vec<_>>();
    let network_targets = targets
        .iter()
        .filter_map(|workspace| {
            workspace
                .assigned_ip
                .clone()
                .map(|ip| (ip, workspace.id.clone()))
        })
        .collect::<Vec<_>>();
    let network_owner_ids = targets
        .iter()
        .filter(|workspace| workspace.assigned_ip.is_some())
        .map(|workspace| workspace.id.clone())
        .collect::<BTreeSet<_>>();
    let mut network_cleanup = None;
    let stop_result = session::stop_sessions_batch_with_hook(&stop_targets, || {
        let network_targets = network_targets.clone();
        network_cleanup = Some(thread::spawn(move || {
            crate::network::teardown_workspace_networks(&network_targets)
        }));
    });
    let network_reports = if let Some(handle) = network_cleanup {
        match handle.join() {
            Ok(reports) => reports,
            Err(error) => {
                tracing::error!("workspace network cleanup thread panicked: {:?}", error);
                network_targets
                    .iter()
                    .map(|(ip, id)| network::NetworkCleanupReport {
                        workspace_id: id.clone(),
                        assigned_ip: ip.clone(),
                        veth_host: None,
                        anti_spoof_rules_absent: false,
                        veth_absent: false,
                        failures: vec![network::NetworkCleanupFailure {
                            resource: "cleanup-worker".to_string(),
                            message: "network cleanup thread panicked".to_string(),
                        }],
                    })
                    .collect()
            }
        }
    } else {
        Vec::new()
    };
    let stop_result = stop_result?;

    let mut stopped_ids = BTreeSet::new();
    let mut cleanup_jobs = Vec::new();
    let mut failed_ids = Vec::new();
    for workspace in &targets {
        let failed = workspace
            .runtime_pid
            .is_some_and(|pid| stop_result.failed_pids.contains(&pid));
        if failed {
            failed_ids.push(workspace.id.clone());
        } else {
            stopped_ids.insert(workspace.id.clone());
        }
    }
    for report in network_reports {
        if !report.is_complete() {
            if network_owner_ids.contains(&report.workspace_id) {
                stopped_ids.remove(&report.workspace_id);
            }
            failed_ids.push(cleanup::format_network_cleanup_error(&report));
        }
    }

    if !stopped_ids.is_empty() {
        cleanup_jobs = with_registry(state_dir, |registry| {
            let sandbox_id = resolve_sandbox_id(registry, sandbox_selector)?;
            let sandbox = registry
                .sandboxes
                .get(&sandbox_id)
                .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_id))?;
            Ok(stopped_ids
                .iter()
                .filter_map(|workspace_id| {
                    sandbox
                        .workspaces
                        .get(workspace_id)
                        .map(|workspace| WorkspaceStopCleanup {
                            sandbox: sandbox.metadata.clone(),
                            workspace: workspace.clone(),
                        })
                })
                .collect::<Vec<_>>())
        })?;
    }

    let cleanup_errors = cleanup::run_workspace_stop_cleanups(cleanup_jobs, true);
    let failed_cleanup_ids = cleanup_errors
        .iter()
        .filter_map(|error| error.split_once(':').map(|(id, _)| id.to_string()))
        .collect::<BTreeSet<_>>();
    failed_ids.extend(cleanup_errors);
    let confirmed_stopped_ids = stopped_ids
        .difference(&failed_cleanup_ids)
        .cloned()
        .collect::<BTreeSet<_>>();

    if !confirmed_stopped_ids.is_empty() {
        with_registry_mut(state_dir, |registry| {
            let sandbox_id = resolve_sandbox_id(registry, sandbox_selector)?;
            let sandbox = registry
                .sandboxes
                .get_mut(&sandbox_id)
                .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_id))?;
            for workspace_id in &confirmed_stopped_ids {
                mark_workspace_stopped(sandbox, workspace_id)?;
            }
            Ok(())
        })?;
    }

    with_registry(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, sandbox_selector)?;
        let sandbox = registry
            .sandboxes
            .get(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_selector))?;
        remove_sandbox_cgroup_if_idle(sandbox);
        Ok(())
    })?;

    Ok(failed_ids)
}

pub(crate) fn freeze_workspaces_in_sandbox(
    state_dir: &std::path::Path,
    sandbox_selector: &str,
    frozen: bool,
) -> Result<()> {
    let (sandbox_id, has_active_workspace) = with_registry(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, sandbox_selector)?;
        let sandbox = registry
            .sandboxes
            .get(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_selector))?;
        Ok((
            sandbox_id,
            sandbox
                .workspaces
                .values()
                .filter(|workspace| workspace.status.may_have_runtime())
                .count()
                > 0,
        ))
    })?;

    if !has_active_workspace {
        return Ok(());
    }
    let cgroup_path = std::path::PathBuf::from("/sys/fs/cgroup")
        .join(crate::sandbox::cgroup::sandbox_cgroup_name(&sandbox_id));
    if !crate::sandbox::cgroup::set_cgroup_frozen(&cgroup_path, frozen)? {
        bail!(
            "sandbox cgroup {} does not support freezing",
            cgroup_path.display()
        );
    }
    Ok(())
}
