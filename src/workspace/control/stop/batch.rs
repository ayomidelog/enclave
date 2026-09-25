//! Stopping every workspace in a sandbox in one pass.
//!
//! The runtimes are signalled together and their network teardown starts as soon
//! as the first signal is sent, because a sandbox stop is dominated by waiting
//! for runtimes to exit and by the per-workspace host cleanup. What remains is
//! per-workspace bookkeeping, which is committed one workspace at a time so a
//! later failure cannot retain an already-cleaned record.

use super::*;

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
