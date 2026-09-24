use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{anyhow, Context, Result};

use crate::network;
use crate::network::NetworkCleanupReport;

use super::{remove_workspace_cgroups, WorkspaceStopCleanup};

/// Tear down many workspaces in one pass.
///
/// The shared steps — network teardown, `/tmp` reset, and the storage unmount —
/// run once for the whole batch so a sandbox stop does not read mountinfo or
/// shell out once per workspace.
pub(crate) fn run_workspace_stop_cleanups(
    cleanups: Vec<WorkspaceStopCleanup>,
    network_already_cleaned: bool,
) -> Vec<String> {
    if cleanups.is_empty() {
        return Vec::new();
    }

    let network_targets = cleanups
        .iter()
        .filter_map(|cleanup| {
            cleanup
                .workspace
                .assigned_ip
                .clone()
                .map(|ip| (ip, cleanup.workspace.id.clone()))
        })
        .collect::<Vec<_>>();
    let network_reports = if network_already_cleaned {
        Vec::new()
    } else {
        crate::network::teardown_workspace_networks(&network_targets)
    };
    let network_failures = Arc::new(
        network_reports
            .into_iter()
            .filter(|report| !report.is_complete())
            .map(|report| {
                (
                    report.workspace_id.clone(),
                    format_network_cleanup_error(&report),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>(),
    );
    // `/tmp` lives inside the workspace disk image, so clear it while the
    // image is still mounted. Doing this after the batch unmount would only
    // empty the host mountpoint and silently leave the image untouched.
    let tmp_reset_failures = Arc::new(
        cleanups
            .iter()
            .filter(|cleanup| cleanup.workspace.clear_tmp_on_restart)
            .filter_map(|cleanup| {
                crate::workspace::reset_workspace_tmp(&cleanup.workspace)
                    .err()
                    .map(|error| {
                        (
                            cleanup.workspace.id.clone(),
                            format!("failed to reset workspace /tmp: {error:#}"),
                        )
                    })
            })
            .collect::<std::collections::BTreeMap<_, _>>(),
    );
    let storage_workspaces = cleanups
        .iter()
        .map(|cleanup| cleanup.workspace.clone())
        .collect::<Vec<_>>();
    // A successful batch unmount already used one consistent mountinfo
    // snapshot. Avoid re-reading mountinfo once per workspace; retain the
    // per-workspace path as a retry fallback when the batch operation fails.
    let storage_batch_succeeded =
        match crate::workspace::ensure_workspace_storage_unmounted_many(&storage_workspaces) {
            Ok(()) => true,
            Err(err) => {
                tracing::warn!("batch workspace storage cleanup failed: {err:#}");
                false
            }
        };

    let worker_count = cleanup_worker_count(cleanups.len());
    let queue = Arc::new(Mutex::new(VecDeque::from(cleanups)));
    let (result_sender, result_receiver) = std::sync::mpsc::channel();
    let handles = (0..worker_count)
        .map(|worker_id| {
            let queue = Arc::clone(&queue);
            let network_failures = Arc::clone(&network_failures);
            let tmp_reset_failures = Arc::clone(&tmp_reset_failures);
            let result_sender = result_sender.clone();
            thread::Builder::new()
                .name(format!("enclave-workspace-cleanup-{worker_id}"))
                .spawn(move || loop {
                    let cleanup = match queue.lock() {
                        Ok(mut queue) => queue.pop_front(),
                        Err(_) => return,
                    };
                    let Some(cleanup) = cleanup else {
                        return;
                    };
                    let workspace_id = cleanup.workspace.id.clone();
                    let mut pre_failures = Vec::new();
                    if let Some(network_error) = network_failures.get(&workspace_id) {
                        pre_failures.push(network_error.clone());
                    }
                    if let Some(tmp_error) = tmp_reset_failures.get(&workspace_id) {
                        pre_failures.push(tmp_error.clone());
                    }
                    if !pre_failures.is_empty() {
                        let _ = result_sender
                            .send((workspace_id, Err(anyhow!("{}", pre_failures.join("; ")))));
                        continue;
                    }
                    let result = run_workspace_stop_cleanup(
                        cleanup,
                        network_already_cleaned,
                        storage_batch_succeeded,
                    );
                    let _ = result_sender.send((workspace_id, result));
                })
                .expect("failed to spawn workspace cleanup worker")
        })
        .collect::<Vec<_>>();

    for handle in handles {
        if let Err(err) = handle.join() {
            tracing::warn!("workspace cleanup thread panicked: {:?}", err);
        }
    }
    drop(result_sender);
    result_receiver
        .into_iter()
        .filter_map(|(workspace_id, result)| {
            result
                .err()
                .map(|error| format!("{workspace_id}: {error:#}"))
        })
        .collect()
}

pub(crate) fn format_network_cleanup_error(report: &network::NetworkCleanupReport) -> String {
    format!(
        "{}: network cleanup incomplete ({})",
        report.workspace_id,
        report
            .failures
            .iter()
            .map(|failure| format!("{}: {}", failure.resource, failure.message))
            .collect::<Vec<_>>()
            .join("; ")
    )
}

fn cleanup_worker_count(job_count: usize) -> usize {
    std::env::var("ENCLAVE_CLEANUP_WORKERS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| (1..=64).contains(value))
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|parallelism| parallelism.get().clamp(1, 4))
                .unwrap_or(4)
        })
        .min(job_count)
}

/// Release the runtime, cgroups, network, storage, and `/tmp` for one workspace.
///
/// This is the stop path: it leaves the workspace files and registry record in
/// place. It returns the network report so the caller can fold the network
/// outcome into the cleanup certificate.
pub(crate) fn run_workspace_stop_cleanup(
    cleanup: WorkspaceStopCleanup,
    network_already_cleaned: bool,
    storage_already_unmounted: bool,
) -> Result<Option<NetworkCleanupReport>> {
    let workspace = cleanup.workspace;
    let sandbox = cleanup.sandbox;

    let cgroups = crate::perf::Timer::new("workspace.stop.cgroups");
    remove_workspace_cgroups(&sandbox, &workspace.id, workspace.runtime_pid)?;
    drop(cgroups);

    let mut network_report = None;
    if !network_already_cleaned {
        if let Some(ip) = workspace.assigned_ip.as_deref() {
            let network = crate::perf::Timer::new("workspace.stop.network");
            let report = crate::network::teardown_workspace_network(ip, &workspace.id);
            drop(network);
            if !report.is_complete() {
                return Err(anyhow!("{}", format_network_cleanup_error(&report)));
            }
            network_report = Some(report);
        }
    }

    if storage_already_unmounted {
        return Ok(network_report);
    }

    // `/tmp` is backed by the workspace disk image, so it must be cleared
    // before that image is unmounted.
    if workspace.clear_tmp_on_restart {
        let reset = crate::perf::Timer::new("workspace.stop.tmp_reset");
        crate::workspace::reset_workspace_tmp(&workspace)
            .with_context(|| format!("failed to reset workspace /tmp for {}", workspace.id))?;
        drop(reset);
    }
    let unmount = crate::perf::Timer::new("workspace.stop.unmount");
    crate::workspace::ensure_workspace_storage_unmounted(&workspace)
        .context("failed to unmount workspace storage")?;
    drop(unmount);
    Ok(network_report)
}
