use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use nix::sched::{setns, CloneFlags};

use crate::workspace::{validate_published_ports, PublishedPortSpec, PublishedPortStatus};

mod limiter;
mod proxy;
mod publication;

use limiter::{ConnectionBudget, ConnectionLimiter};
use proxy::run_accept_loop;
use publication::{shutdown_publications, ActivePublication, PublishMode, WorkspacePublishKey};

const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(100);
const CONNECTION_POLL_INTERVAL: Duration = Duration::from_millis(100);
/// How long a proxied connection may move no bytes before it is closed.
///
/// Published ports are ordinary TCP services, so a connection that goes quiet
/// is either an idle keep-alive or an abandoned socket. Ten minutes is long
/// enough to leave a genuinely idle client alone and short enough that
/// abandoned sockets cannot hold the connection budgets forever.
const CONNECTION_IDLE_TIMEOUT: Duration = Duration::from_secs(600);
/// Per published port. Kept as the tighter bound so one noisy port cannot
/// dominate the daemon.
const MAX_CONNECTIONS_PER_PUBLISHER: usize = 128;
/// Daemon-wide across every published port, so the total number of proxied
/// connections is bounded no matter how many ports are published.
const MAX_PUBLISHED_CONNECTIONS: usize = 1024;

/// One workspace the publisher is serving ports for, and the ports it holds.
#[derive(Debug, Clone)]
pub struct PublishedPortOwner {
    pub sandbox_id: String,
    pub workspace_id: String,
    pub ports: Vec<PublishedPortStatus>,
}

pub struct PortPublisher {
    inner: Mutex<PublisherState>,
    connections: Arc<ConnectionLimiter>,
}

impl Default for PortPublisher {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Default)]
struct PublisherState {
    active: BTreeMap<WorkspacePublishKey, Vec<ActivePublication>>,
    failed: BTreeMap<WorkspacePublishKey, Vec<PublishedPortStatus>>,
}

impl std::fmt::Debug for PortPublisher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.inner.lock().expect("port publisher mutex poisoned");
        f.debug_struct("PortPublisher")
            .field("active_workspaces", &state.active.len())
            .field("failed_workspaces", &state.failed.len())
            .finish()
    }
}

impl PortPublisher {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(PublisherState::default()),
            connections: Arc::new(ConnectionLimiter::new(MAX_PUBLISHED_CONNECTIONS)),
        }
    }

    pub fn apply_workspace_ports_strict(
        &self,
        sandbox_id: &str,
        workspace_id: &str,
        runtime_pid: u32,
        workspace_ip: &str,
        specs: &[PublishedPortSpec],
    ) -> Result<Vec<PublishedPortStatus>> {
        self.apply_workspace_ports(
            sandbox_id,
            workspace_id,
            runtime_pid,
            workspace_ip,
            specs,
            PublishMode::Strict,
        )
    }

    pub fn reconcile_workspace_ports(
        &self,
        sandbox_id: &str,
        workspace_id: &str,
        runtime_pid: u32,
        workspace_ip: &str,
        specs: &[PublishedPortSpec],
    ) -> Result<Vec<PublishedPortStatus>> {
        self.apply_workspace_ports(
            sandbox_id,
            workspace_id,
            runtime_pid,
            workspace_ip,
            specs,
            PublishMode::BestEffort,
        )
    }

    pub fn clear_workspace_ports(&self, sandbox_id: &str, workspace_id: &str) {
        let key = WorkspacePublishKey::new(sandbox_id, workspace_id);
        let active = self.take_active_publications(&key);
        shutdown_publications(active);
    }

    /// Whether any published port for a workspace is still serving.
    ///
    /// `clear_workspace_ports` returns nothing, so a caller that wants to prove
    /// the listeners were released has to look at the publisher afterwards. This
    /// is what lets a stop certificate cover ports rather than assume them.
    pub fn has_active_workspace_ports(&self, sandbox_id: &str, workspace_id: &str) -> bool {
        let key = WorkspacePublishKey::new(sandbox_id, workspace_id);
        let state = self.inner.lock().expect("port publisher mutex poisoned");
        state
            .active
            .get(&key)
            .is_some_and(|publications| !publications.is_empty())
    }

    /// Every workspace the publisher is currently serving ports for.
    ///
    /// The publisher is the only authority on which host ports Enclave holds:
    /// the listeners are owned by threads rather than by any file on disk, so a
    /// caller that wants to prove nothing was left behind has to ask here. Doctor
    /// uses this to name a listener whose workspace is no longer running, which
    /// is otherwise invisible until a later start fails with the port in use.
    pub fn active_workspaces(&self) -> Vec<PublishedPortOwner> {
        let state = self.inner.lock().expect("port publisher mutex poisoned");
        state
            .active
            .iter()
            .map(|(key, publications)| PublishedPortOwner {
                sandbox_id: key.sandbox_id.clone(),
                workspace_id: key.workspace_id.clone(),
                ports: publications.iter().map(ActivePublication::status).collect(),
            })
            .collect()
    }

    pub fn workspace_statuses(
        &self,
        sandbox_id: &str,
        workspace_id: &str,
    ) -> Vec<PublishedPortStatus> {
        let key = WorkspacePublishKey::new(sandbox_id, workspace_id);
        let state = self.inner.lock().expect("port publisher mutex poisoned");

        let mut statuses = Vec::new();
        if let Some(active) = state.active.get(&key) {
            statuses.extend(active.iter().map(ActivePublication::status));
        }
        if let Some(failed) = state.failed.get(&key) {
            statuses.extend(failed.iter().cloned());
        }
        statuses.sort_by_key(|status| {
            (
                status.host_ip.clone(),
                status.host_port,
                status.workspace_port,
                status.protocol.clone(),
            )
        });
        statuses
    }

    fn apply_workspace_ports(
        &self,
        sandbox_id: &str,
        workspace_id: &str,
        runtime_pid: u32,
        workspace_ip: &str,
        specs: &[PublishedPortSpec],
        mode: PublishMode,
    ) -> Result<Vec<PublishedPortStatus>> {
        validate_published_ports(specs)?;

        let key = WorkspacePublishKey::new(sandbox_id, workspace_id);
        let old_active = self.take_active_publications(&key);
        shutdown_publications(old_active);

        let mut active = Vec::new();
        let mut failures = Vec::new();

        for spec in specs {
            match ActivePublication::bind(
                spec.clone(),
                runtime_pid,
                workspace_ip,
                Arc::clone(&self.connections),
            ) {
                Ok(publication) => active.push(publication),
                Err(err) => match mode {
                    PublishMode::Strict => {
                        shutdown_publications(active);
                        self.clear_failed_statuses(&key);
                        return Err(err);
                    }
                    PublishMode::BestEffort => {
                        let message = err.to_string();
                        tracing::warn!(
                            "failed to republish {} for workspace {} in sandbox {}: {message}",
                            spec,
                            workspace_id,
                            sandbox_id
                        );
                        failures.push(PublishedPortStatus::failed(spec, message));
                    }
                },
            }
        }

        let statuses = active
            .iter()
            .map(ActivePublication::status)
            .chain(failures.iter().cloned())
            .collect::<Vec<_>>();
        self.store_workspace_state(key, active, failures);
        Ok(statuses)
    }

    fn take_active_publications(&self, key: &WorkspacePublishKey) -> Vec<ActivePublication> {
        let mut state = self.inner.lock().expect("port publisher mutex poisoned");
        let active = state.active.remove(key).unwrap_or_default();
        state.failed.remove(key);
        active
    }

    fn clear_failed_statuses(&self, key: &WorkspacePublishKey) {
        let mut state = self.inner.lock().expect("port publisher mutex poisoned");
        state.failed.remove(key);
    }

    fn store_workspace_state(
        &self,
        key: WorkspacePublishKey,
        active: Vec<ActivePublication>,
        failures: Vec<PublishedPortStatus>,
    ) {
        let mut state = self.inner.lock().expect("port publisher mutex poisoned");
        if active.is_empty() {
            state.active.remove(&key);
        } else {
            state.active.insert(key.clone(), active);
        }
        if failures.is_empty() {
            state.failed.remove(&key);
        } else {
            state.failed.insert(key, failures);
        }
    }
}

fn publish_bind_error(spec: &PublishedPortSpec, err: io::Error) -> anyhow::Error {
    if err.kind() == io::ErrorKind::AddrInUse {
        return anyhow::anyhow!(
            "failed to publish {}:{} -> workspace port {}: host port already in use",
            spec.host_ip,
            spec.host_port,
            spec.workspace_port
        );
    }

    anyhow::anyhow!(
        "failed to publish {}:{} -> workspace port {}: {}",
        spec.host_ip,
        spec.host_port,
        spec.workspace_port,
        err
    )
}

// The publish tests exercise the connection copier and activity tracker
// directly.
#[cfg(test)]
use proxy::{copy_until_shutdown, ConnectionActivity};

#[cfg(test)]
#[path = "../../../tests/src/network/publish.rs"]
mod tests;
