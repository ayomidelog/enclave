//! The background activity sweep.
//!
//! The sweep is deliberately a daemon loop rather than a request path. It wakes
//! on a bounded interval, asks the activity service for work that is genuinely
//! idle, and uses the same lifecycle functions a CLI stop or destroy would use.
//! A lease held by an in-flight request is checked immediately before each
//! action, so a request that arrives during a sweep wins over an idle decision.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::Result;

use crate::workspace::activity_service;
use crate::workspace::WorkspaceStatus;

use super::leases::LeaseScope;
use super::services::DaemonServices;
use super::DaemonConfig;

/// Spawn the sweeper. The returned handle is joined during daemon shutdown.
pub(super) fn spawn(
    config: DaemonConfig,
    services: DaemonServices,
    shutdown: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("enclave-idle-sweeper".to_string())
        .spawn(move || {
            let interval = services.policy.sweep_interval;
            while !shutdown.load(Ordering::SeqCst) {
                if let Err(error) = sweep(&config, &services) {
                    tracing::warn!("idle sweep failed: {error:#}");
                }
                wait_or_shutdown(interval, &shutdown);
            }
        })
        .expect("failed to spawn the idle sweeper")
}

fn wait_or_shutdown(interval: Duration, shutdown: &AtomicBool) {
    let deadline = std::time::Instant::now() + interval;
    while !shutdown.load(Ordering::SeqCst) {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return;
        }
        thread::sleep(remaining.min(Duration::from_millis(250)));
    }
}

/// Run one sweep. This is public within the daemon so tests can drive it
/// without waiting for the background interval.
pub(crate) fn sweep(config: &DaemonConfig, services: &DaemonServices) -> Result<()> {
    sweep_at(config, services, chrono::Utc::now())
}

/// Run one sweep against an explicit clock.
///
/// Tests use this to make a workspace idle without sleeping for its configured
/// threshold. Production always reaches it through [`sweep`].
pub(crate) fn sweep_at(
    config: &DaemonConfig,
    services: &DaemonServices,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    let policy = &services.policy;
    for workspace in activity_service::running_workspaces(&config.state_dir, None)? {
        if let Err(error) = activity_service::reconcile_workspace_jobs(
            &config.state_dir,
            &services.activity,
            &workspace.sandbox_id,
            &workspace.id,
        ) {
            tracing::warn!(
                workspace = %workspace.id,
                "failed to refresh durable jobs before sweep: {error:#}"
            );
        }
    }
    let expired = services
        .activity
        .reap_expired_jobs(&config.state_dir, now)?;
    for (workspace_id, job_id) in expired {
        tracing::info!(
            workspace = %workspace_id,
            job = %job_id,
            "background job TTL expired; signalling its process group"
        );
    }
    let candidates = if policy.evict_idle {
        activity_service::idle_candidates(
            &config.state_dir,
            &services.activity,
            policy.idle_stop_after_secs,
            now,
        )?
    } else {
        Vec::new()
    };
    for workspace in candidates {
        // The candidate list is a snapshot. Take the same lifecycle lease a
        // normal stop takes before re-reading it, so a start, stop, resize, or
        // destroy cannot change the record while this sweep is acting on it.
        // The lease is deliberately acquired before the re-read: checking first
        // and acquiring second leaves exactly the race the lease exists to close.
        let _lease = match services.leases.acquire(LeaseScope::Workspace {
            sandbox: workspace.sandbox_id.clone(),
            workspace: workspace.id.clone(),
        }) {
            Ok(lease) => lease,
            Err(error) => {
                tracing::warn!(
                    workspace = %workspace.id,
                    "idle sweep could not acquire the workspace lifecycle lease: {error:#}"
                );
                continue;
            }
        };
        let current = match crate::workspace::workspace_metadata(
            &config.state_dir,
            &workspace.sandbox_id,
            &workspace.id,
        ) {
            Ok(workspace) => workspace,
            Err(_) => continue,
        };
        if current.status != WorkspaceStatus::Running
            || services
                .activity
                .active_commands(&current.sandbox_id, &current.id)
                > 0
            || services
                .activity
                .has_active_foreground_job(&current.sandbox_id, &current.id)
            || services
                .port_publisher
                .has_live_workspace_connections(&current.sandbox_id, &current.id)
            || services
                .activity
                .keeps_workspace_running(&current.sandbox_id, &current.id)
        {
            continue;
        }
        if let Some(threshold) =
            crate::workspace::idle_threshold(&current, policy.idle_stop_after_secs)
        {
            if !crate::workspace::is_idle(
                &current,
                threshold,
                now,
                services
                    .activity
                    .active_commands(&current.sandbox_id, &current.id),
            ) {
                continue;
            }
        }
        tracing::info!(
            workspace = %current.id,
            sandbox = %current.sandbox_id,
            "stopping idle workspace"
        );
        match crate::workspace::stop_workspace_with_certificate(
            &config.state_dir,
            &current.sandbox_id,
            &current.id,
        ) {
            Ok((metadata, certificate)) => {
                // The stop path cancels the jobs only after the runtime cleanup
                // has succeeded. Pruning here only removes terminal entries from
                // the live map; the durable records remain for `workspace jobs`.
                services
                    .activity
                    .prune_terminal_jobs(&metadata.sandbox_id, &metadata.id);
                services
                    .port_publisher
                    .clear_workspace_ports(&metadata.sandbox_id, &metadata.id);
                if !certificate.is_complete() {
                    tracing::warn!(
                        workspace = %metadata.id,
                        "idle stop left resources behind: {}",
                        certificate.failure_summary()
                    );
                }
            }
            Err(error) => tracing::warn!(
                workspace = %current.id,
                "idle stop failed: {error:#}"
            ),
        }
    }

    let expired_workspaces =
        activity_service::destroy_candidates(&config.state_dir, policy.destroy_after_secs, now)?;
    for workspace in expired_workspaces {
        // A destroy is a lifecycle mutation too. A workspace may have been
        // started or pinned between building the candidate list and this loop,
        // so serialize and re-evaluate the current record under the lease.
        let _lease = match services.leases.acquire(LeaseScope::Workspace {
            sandbox: workspace.sandbox_id.clone(),
            workspace: workspace.id.clone(),
        }) {
            Ok(lease) => lease,
            Err(error) => {
                tracing::warn!(
                    workspace = %workspace.id,
                    "destroy-after sweep could not acquire the workspace lifecycle lease: {error:#}"
                );
                continue;
            }
        };
        let current = match crate::workspace::workspace_metadata(
            &config.state_dir,
            &workspace.sandbox_id,
            &workspace.id,
        ) {
            Ok(current) => current,
            Err(_) => continue,
        };
        if current.status != WorkspaceStatus::Stopped
            || current.keep_running
            || services
                .activity
                .active_commands(&current.sandbox_id, &current.id)
                > 0
        {
            continue;
        }
        let Some(threshold) =
            crate::workspace::destroy_threshold(&current, policy.destroy_after_secs)
        else {
            continue;
        };
        let expired = current
            .last_stopped_at
            .as_deref()
            .and_then(crate::workspace::parse_timestamp)
            .is_some_and(|stopped| (now - stopped).num_seconds() >= threshold as i64);
        if threshold == 0 || !expired {
            continue;
        }
        tracing::info!(
            workspace = %current.id,
            sandbox = %current.sandbox_id,
            "destroying workspace after its retention window"
        );
        if let Err(error) = crate::workspace::destroy_workspace_with_mode(
            &config.state_dir,
            &current.sandbox_id,
            &current.id,
            crate::workspace::CleanupMode::Normal,
        ) {
            tracing::warn!(
                workspace = %current.id,
                "destroy-after failed: {error:#}"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "../../tests/src/daemon/sweeper.rs"]
mod tests;
