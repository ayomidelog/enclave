use std::collections::BTreeMap;
use std::fs::File;
use std::io;
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result};
use nix::sched::{setns, CloneFlags};

use crate::workspace::{validate_published_ports, PublishedPortSpec, PublishedPortStatus};

const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Default)]
pub struct PortPublisher {
    inner: Mutex<PublisherState>,
}

#[derive(Default)]
struct PublisherState {
    active: BTreeMap<WorkspacePublishKey, Vec<ActivePublication>>,
    failed: BTreeMap<WorkspacePublishKey, Vec<PublishedPortStatus>>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct WorkspacePublishKey {
    sandbox_id: String,
    workspace_id: String,
}

struct ActivePublication {
    spec: PublishedPortSpec,
    workspace_ip: String,
    shutdown: Arc<AtomicBool>,
    accept_thread: Option<JoinHandle<()>>,
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
        Self::default()
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
            match ActivePublication::bind(spec.clone(), runtime_pid, workspace_ip) {
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

impl WorkspacePublishKey {
    fn new(sandbox_id: &str, workspace_id: &str) -> Self {
        Self {
            sandbox_id: sandbox_id.to_string(),
            workspace_id: workspace_id.to_string(),
        }
    }
}

impl ActivePublication {
    fn bind(spec: PublishedPortSpec, runtime_pid: u32, workspace_ip: &str) -> Result<Self> {
        let bind_addr = format!("{}:{}", spec.host_ip, spec.host_port);
        let listener =
            TcpListener::bind(&bind_addr).map_err(|err| publish_bind_error(&spec, err))?;
        listener
            .set_nonblocking(true)
            .with_context(|| format!("failed to configure nonblocking listener at {bind_addr}"))?;

        let shutdown = Arc::new(AtomicBool::new(false));
        let accept_shutdown = shutdown.clone();
        let thread_name = format!("enclave-port-{}-{}", spec.host_port, spec.workspace_port);
        let workspace_port = spec.workspace_port;
        let accept_thread = thread::Builder::new()
            .name(thread_name)
            .spawn(move || run_accept_loop(listener, accept_shutdown, runtime_pid, workspace_port))
            .context("failed to spawn published-port accept thread")?;

        Ok(Self {
            spec,
            workspace_ip: workspace_ip.to_string(),
            shutdown,
            accept_thread: Some(accept_thread),
        })
    }

    fn status(&self) -> PublishedPortStatus {
        PublishedPortStatus::active(&self.spec, &self.workspace_ip)
    }

    fn shutdown(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(handle) = self.accept_thread.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for ActivePublication {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[derive(Clone, Copy)]
enum PublishMode {
    Strict,
    BestEffort,
}

fn shutdown_publications(mut publications: Vec<ActivePublication>) {
    for publication in &mut publications {
        publication.shutdown();
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

fn run_accept_loop(
    listener: TcpListener,
    shutdown: Arc<AtomicBool>,
    runtime_pid: u32,
    workspace_port: u16,
) {
    while !shutdown.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                let _ = thread::Builder::new()
                    .name("enclave-port-conn".to_string())
                    .spawn(move || handle_connection(stream, runtime_pid, workspace_port));
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(ACCEPT_POLL_INTERVAL);
            }
            Err(err) => {
                if !shutdown.load(Ordering::SeqCst) {
                    tracing::warn!(
                        "published port accept failed for workspace pid {} port {}: {}",
                        runtime_pid,
                        workspace_port,
                        err
                    );
                }
                thread::sleep(ACCEPT_POLL_INTERVAL);
            }
        }
    }
}

fn handle_connection(mut client_stream: TcpStream, runtime_pid: u32, workspace_port: u16) {
    let mut workspace_stream = match connect_to_workspace_service(runtime_pid, workspace_port) {
        Ok(stream) => stream,
        Err(err) => {
            tracing::debug!(
                "published port connect failed for workspace pid {} port {}: {}",
                runtime_pid,
                workspace_port,
                err
            );
            return;
        }
    };

    let mut client_reader = match client_stream.try_clone() {
        Ok(stream) => stream,
        Err(err) => {
            tracing::warn!("failed to clone published-port client stream: {err}");
            return;
        }
    };
    let mut workspace_writer = match workspace_stream.try_clone() {
        Ok(stream) => stream,
        Err(err) => {
            tracing::warn!("failed to clone published-port workspace stream: {err}");
            return;
        }
    };

    let upstream = thread::spawn(move || {
        let _ = io::copy(&mut client_reader, &mut workspace_writer);
        let _ = workspace_writer.shutdown(Shutdown::Write);
    });

    let _ = io::copy(&mut workspace_stream, &mut client_stream);
    let _ = client_stream.shutdown(Shutdown::Write);
    let _ = upstream.join();
}

fn connect_to_workspace_service(runtime_pid: u32, workspace_port: u16) -> io::Result<TcpStream> {
    if runtime_pid == std::process::id() {
        return TcpStream::connect(("127.0.0.1", workspace_port));
    }

    let netns = File::open(format!("/proc/{runtime_pid}/ns/net"))?;
    setns(&netns, CloneFlags::CLONE_NEWNET).map_err(nix_to_io_error)?;
    TcpStream::connect(("127.0.0.1", workspace_port))
}

fn nix_to_io_error(err: nix::errno::Errno) -> io::Error {
    io::Error::from_raw_os_error(err as i32)
}
