use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result};
use nix::sched::{setns, CloneFlags};

use crate::workspace::{validate_published_ports, PublishedPortSpec, PublishedPortStatus};

const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(100);
const CONNECTION_POLL_INTERVAL: Duration = Duration::from_millis(100);
const MAX_CONNECTIONS_PER_PUBLISHER: usize = 128;

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

struct ConnectionLimiter {
    state: Mutex<ConnectionState>,
    limit: usize,
}

#[derive(Default)]
struct ConnectionState {
    active: usize,
}

struct ConnectionPermit {
    limiter: Arc<ConnectionLimiter>,
}

impl ConnectionLimiter {
    fn new(limit: usize) -> Self {
        Self {
            state: Mutex::new(ConnectionState::default()),
            limit,
        }
    }

    fn try_acquire(self: &Arc<Self>) -> Option<ConnectionPermit> {
        let mut state = self
            .state
            .lock()
            .expect("connection limiter mutex poisoned");
        if state.active >= self.limit {
            return None;
        }
        state.active += 1;
        Some(ConnectionPermit {
            limiter: Arc::clone(self),
        })
    }

    #[cfg(test)]
    fn active(&self) -> usize {
        self.state
            .lock()
            .expect("connection limiter mutex poisoned")
            .active
    }
}

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        let mut state = self
            .limiter
            .state
            .lock()
            .expect("connection limiter mutex poisoned");
        state.active = state.active.saturating_sub(1);
    }
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
            connections: Arc::new(ConnectionLimiter::new(MAX_CONNECTIONS_PER_PUBLISHER)),
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

impl WorkspacePublishKey {
    fn new(sandbox_id: &str, workspace_id: &str) -> Self {
        Self {
            sandbox_id: sandbox_id.to_string(),
            workspace_id: workspace_id.to_string(),
        }
    }
}

impl ActivePublication {
    fn bind(
        spec: PublishedPortSpec,
        runtime_pid: u32,
        workspace_ip: &str,
        connections: Arc<ConnectionLimiter>,
    ) -> Result<Self> {
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
            .spawn(move || {
                run_accept_loop(
                    listener,
                    accept_shutdown,
                    runtime_pid,
                    workspace_port,
                    connections,
                )
            })
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
    connections: Arc<ConnectionLimiter>,
) {
    while !shutdown.load(Ordering::SeqCst) {
        // Wait for a connection instead of polling. Sleeping on `WouldBlock`
        // added the poll interval as a latency floor to the first connection
        // after an idle period, and burned a wakeup per interval per port.
        if !wait_for_accept(&listener, shutdown.as_ref()) {
            return;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                let Some(permit) = connections.try_acquire() else {
                    tracing::debug!(
                        "published port connection limit reached for workspace pid {} port {}; rejecting connection",
                        runtime_pid, workspace_port
                    );
                    let _ = stream.shutdown(Shutdown::Both);
                    continue;
                };
                let connection_shutdown = Arc::clone(&shutdown);
                if let Err(err) = thread::Builder::new()
                    .name("enclave-port-conn".to_string())
                    .spawn(move || {
                        let _permit = permit;
                        handle_connection(stream, runtime_pid, workspace_port, connection_shutdown);
                    })
                {
                    tracing::warn!("failed to spawn published-port connection worker: {err}");
                }
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                // A spurious wakeup between poll and accept is harmless.
                continue;
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

/// Block until the listener has a pending connection or the timeout expires.
/// Returns `false` when the publisher has been asked to shut down.
fn wait_for_accept(listener: &TcpListener, shutdown: &AtomicBool) -> bool {
    loop {
        if shutdown.load(Ordering::SeqCst) {
            return false;
        }
        let mut descriptor = libc::pollfd {
            fd: listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let result =
            unsafe { libc::poll(&mut descriptor, 1, ACCEPT_POLL_INTERVAL.as_millis() as i32) };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            tracing::warn!("published port accept poll failed: {error}");
            return false;
        }
        if result > 0 {
            return true;
        }
    }
}

fn handle_connection(
    mut client_stream: TcpStream,
    runtime_pid: u32,
    workspace_port: u16,
    shutdown: Arc<AtomicBool>,
) {
    let _ = client_stream.set_read_timeout(Some(CONNECTION_POLL_INTERVAL));
    let _ = client_stream.set_write_timeout(Some(CONNECTION_POLL_INTERVAL));
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
    let _ = workspace_stream.set_read_timeout(Some(CONNECTION_POLL_INTERVAL));
    let _ = workspace_stream.set_write_timeout(Some(CONNECTION_POLL_INTERVAL));

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

    let upstream_shutdown = Arc::clone(&shutdown);
    let upstream = thread::spawn(move || {
        let _ = copy_until_shutdown(
            &mut client_reader,
            &mut workspace_writer,
            &upstream_shutdown,
        );
        let _ = workspace_writer.shutdown(Shutdown::Write);
    });

    let _ = copy_until_shutdown(&mut workspace_stream, &mut client_stream, &shutdown);
    let _ = client_stream.shutdown(Shutdown::Write);
    let _ = upstream.join();
}

fn copy_until_shutdown(
    reader: &mut impl Read,
    writer: &mut impl Write,
    shutdown: &AtomicBool,
) -> io::Result<()> {
    let mut buffer = [0u8; 16 * 1024];
    while !shutdown.load(Ordering::SeqCst) {
        match reader.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(read) => writer.write_all(&buffer[..read])?,
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(err) => return Err(err),
        }
    }
    Ok(())
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

#[cfg(test)]
#[path = "../../tests/src/network/publish.rs"]
mod tests;
