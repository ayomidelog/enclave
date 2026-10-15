use anyhow::{anyhow, Context, Result};

use std::sync::Arc;

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

    let workers = crate::parallel::worker_count(
        "ENCLAVE_CLEANUP_WORKERS",
        cleanups.len(),
        crate::parallel::parallelism().clamp(1, 4),
    );
    let outcomes = crate::parallel::map_bounded(cleanups, workers, |cleanup| {
        let workspace_id = cleanup.workspace.id.clone();
        let mut pre_failures = Vec::new();
        if let Some(network_error) = network_failures.get(&workspace_id) {
            pre_failures.push(network_error.clone());
        }
        if let Some(tmp_error) = tmp_reset_failures.get(&workspace_id) {
            pre_failures.push(tmp_error.clone());
        }
        if !pre_failures.is_empty() {
            return (workspace_id, Err(anyhow!("{}", pre_failures.join("; "))));
        }
        let result =
            run_workspace_stop_cleanup(cleanup, network_already_cleaned, storage_batch_succeeded);
        (workspace_id, result)
    });

    outcomes
        .into_iter()
        .flatten()
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

    // Network teardown and storage unmount touch independent host resources: the
    // veth and its rules, and the workspace disk image. Each costs tens of
    // milliseconds of process spawns, and running them one after the other added
    // the network time to every stop, so the network runs alongside the storage
    // work and both outcomes are reported.
    let (network_report, storage_result) = std::thread::scope(|scope| {
        let network = if !network_already_cleaned {
            workspace.assigned_ip.as_deref().map(|ip| {
                let workspace_id = workspace.id.as_str();
                scope.spawn(move || {
                    let timer = crate::perf::Timer::new("workspace.stop.network");
                    let report = crate::network::teardown_workspace_network(ip, workspace_id);
                    drop(timer);
                    report
                })
            })
        } else {
            None
        };

        let storage = if storage_already_unmounted {
            Ok(())
        } else {
            release_workspace_storage(&workspace)
        };

        let network_report = network.map(|handle| {
            handle
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
        });
        (network_report, storage)
    });

    if let Some(report) = network_report.as_ref() {
        if !report.is_complete() {
            // Report the storage outcome too: an operator fixing the network
            // should not have to run the stop again to discover the mount state.
            let network_error = format_network_cleanup_error(report);
            return match storage_result {
                Ok(()) => Err(anyhow!("{network_error}")),
                Err(storage_error) => Err(anyhow!(
                    "{network_error}; storage cleanup also failed: {storage_error:#}"
                )),
            };
        }
    }
    storage_result?;
    Ok(network_report)
}

/// Clear the workspace tmp directory if asked, then unmount its storage.
///
/// The tmp directory is backed by the workspace disk image, so it must be cleared
/// before that image is unmounted.
fn release_workspace_storage(workspace: &crate::workspace::WorkspaceMetadata) -> Result<()> {
    if workspace.clear_tmp_on_restart {
        let reset = crate::perf::Timer::new("workspace.stop.tmp_reset");
        crate::workspace::reset_workspace_tmp(workspace)
            .with_context(|| format!("failed to reset workspace tmp for {}", workspace.id))?;
        drop(reset);
    }
    let unmount = crate::perf::Timer::new("workspace.stop.unmount");
    crate::workspace::ensure_workspace_storage_unmounted(workspace)
        .context("failed to unmount workspace storage")?;
    drop(unmount);
    Ok(())
}
