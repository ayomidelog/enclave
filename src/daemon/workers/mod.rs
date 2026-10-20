use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};

use super::request::handle_client;
use super::DaemonConfig;

const CONTROL_WORKER_COUNT: usize = 6;
const TRANSFER_WORKER_COUNT: usize = 2;
const EXEC_WORKER_COUNT: usize = 16;
const CONTROL_QUEUE_CAPACITY: usize = 128;
const TRANSFER_QUEUE_CAPACITY: usize = 8;
// A pending request is a socket and a blocked thread, so the queue is cheap and
// should absorb a burst rather than reject it. Sixty-four was too small: every
// heartbeat chat in a deployment ticks on the same boundary, and the resulting
// simultaneous workspace commands overflowed it, which surfaced to agents as
// failed tool calls. The bound still exists so a runaway caller cannot queue
// without limit.
const EXEC_QUEUE_CAPACITY: usize = 512;

/// How many exec workers one workspace may occupy at once.
///
/// A workspace's commands serialize on its helper anyway, so more than a couple in
/// flight buys nothing. Reserving the rest for other workspaces is what keeps one
/// busy workspace from starving everyone else.
const EXEC_PER_WORKSPACE: usize = 2;

struct RequestJob {
    stream: UnixStream,
    /// The workspace this request targets, when it is an exec. The worker that
    /// serves it releases this workspace's slot when the request finishes.
    workspace_key: Option<String>,
}

pub(super) struct RequestWorkerPool {
    control_sender: SyncSender<RequestJob>,
    transfer_sender: SyncSender<RequestJob>,
    exec_sender: SyncSender<RequestJob>,
    /// One exec worker reserved for every workspace that is running a command.
    ///
    /// The pool is shared by every workspace, while each workspace serializes on
    /// its own command mutex. Without a per-workspace cap, one workspace issuing
    /// several slow commands could hold most of the pool while a second workspace
    /// starved - which is how a single bad workspace turned into a queue that
    /// rejected everybody's requests. A workspace may use at most `exec_per_workspace`
    /// slots; past that its request waits in the queue instead of taking a worker.
    exec_in_flight: Arc<Mutex<std::collections::HashMap<String, usize>>>,
    exec_per_workspace: usize,
    workers: Vec<thread::JoinHandle<()>>,
}

impl RequestWorkerPool {
    pub(super) fn new(
        config: &DaemonConfig,
        shutdown: &Arc<AtomicBool>,
        services: &super::services::DaemonServices,
    ) -> Result<Self> {
        // The pool is long-lived rather than per-batch, so the count is the configured
        // one and is not bounded by a number of jobs.
        let control_count =
            crate::parallel::env_worker_count("ENCLAVE_CONTROL_WORKERS", CONTROL_WORKER_COUNT);
        let transfer_count =
            crate::parallel::env_worker_count("ENCLAVE_TRANSFER_WORKERS", TRANSFER_WORKER_COUNT);
        // Commands are the one class that can occupy a worker for minutes, so they
        // get their own pool: a stuck command costs one exec slot instead of one of
        // the six control threads that answer `ping` and lifecycle calls.
        let exec_count =
            crate::parallel::env_worker_count("ENCLAVE_EXEC_WORKERS", EXEC_WORKER_COUNT);
        let (control_sender, control_receiver) = mpsc::sync_channel(CONTROL_QUEUE_CAPACITY);
        let (transfer_sender, transfer_receiver) = mpsc::sync_channel(TRANSFER_QUEUE_CAPACITY);
        let (exec_sender, exec_receiver) = mpsc::sync_channel(EXEC_QUEUE_CAPACITY);
        let control_receiver = Arc::new(Mutex::new(control_receiver));
        let transfer_receiver = Arc::new(Mutex::new(transfer_receiver));
        let exec_receiver = Arc::new(Mutex::new(exec_receiver));
        let exec_in_flight: Arc<Mutex<std::collections::HashMap<String, usize>>> =
            Arc::new(Mutex::new(std::collections::HashMap::new()));
        let mut workers = Vec::with_capacity(control_count + transfer_count + exec_count);
        workers.extend(spawn_workers(
            control_receiver,
            control_count,
            "control",
            config,
            shutdown,
            services,
            Arc::clone(&exec_in_flight),
        )?);
        workers.extend(spawn_workers(
            transfer_receiver,
            transfer_count,
            "transfer",
            config,
            shutdown,
            services,
            Arc::clone(&exec_in_flight),
        )?);
        workers.extend(spawn_workers(
            exec_receiver,
            exec_count,
            "exec",
            config,
            shutdown,
            services,
            Arc::clone(&exec_in_flight),
        )?);
        Ok(Self {
            control_sender,
            transfer_sender,
            exec_sender,
            exec_in_flight,
            exec_per_workspace: EXEC_PER_WORKSPACE,
            workers,
        })
    }

    /// Hand a connection to the pool that serves it, never blocking the accept loop.
    ///
    /// A blocking `send` here would turn a full queue into an outage of its own: the
    /// accept loop would stop accepting, so a saturated exec pool would also stop
    /// answering `ping`. When a queue is full the client is told the daemon is busy
    /// and the connection is closed, which the caller can retry.
    pub(super) fn submit(&self, stream: UnixStream) -> Result<()> {
        let class = classify_stream(&stream);
        // Exec requests carry the workspace they target, so a workspace already at
        // its share waits in the queue rather than taking a worker another
        // workspace needs.
        let workspace_key = if class == RequestClass::Exec {
            peek_workspace_key(&stream)
        } else {
            None
        };
        let (sender, capacity) = match class {
            RequestClass::Transfer => (&self.transfer_sender, TRANSFER_QUEUE_CAPACITY),
            RequestClass::Exec => (&self.exec_sender, EXEC_QUEUE_CAPACITY),
            RequestClass::Control => (&self.control_sender, CONTROL_QUEUE_CAPACITY),
        };
        let admission = workspace_key
            .as_deref()
            .map(|key| self.admit_exec(key))
            .unwrap_or(true);
        if !admission {
            // Over this workspace's share. Queue it behind the others instead of
            // handing it a worker; it is admitted by the worker that finishes.
            return self.enqueue_limited(stream, class);
        }
        match sender.try_send(RequestJob {
            stream,
            workspace_key: workspace_key.clone(),
        }) {
            Ok(()) => Ok(()),
            Err(mpsc::TrySendError::Full(job)) => {
                if let Some(key) = workspace_key.as_deref() {
                    self.release_exec(key);
                }
                tracing::warn!(
                    class = ?class,
                    capacity,
                    "daemon request queue is full; rejecting the connection"
                );
                reject_busy(job.stream, class, capacity);
                Ok(())
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                if let Some(key) = workspace_key.as_deref() {
                    self.release_exec(key);
                }
                Err(anyhow::anyhow!("daemon {class:?} queue is closed"))
            }
        }
    }

    /// Take one of a workspace's exec slots, or report that it has none left.
    fn admit_exec(&self, workspace_key: &str) -> bool {
        let mut in_flight = self
            .exec_in_flight
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let count = in_flight.entry(workspace_key.to_string()).or_insert(0);
        if *count >= self.exec_per_workspace {
            return false;
        }
        *count += 1;
        true
    }

    /// Give a workspace's exec slot back when its command finishes.
    fn release_exec(&self, workspace_key: &str) {
        let mut in_flight = self
            .exec_in_flight
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(count) = in_flight.get_mut(workspace_key) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                in_flight.remove(workspace_key);
            }
        }
    }

    /// Queue a request that is over its workspace's share, refusing only when the
    /// queue itself is full.
    fn enqueue_limited(&self, stream: UnixStream, class: RequestClass) -> Result<()> {
        let (sender, capacity) = match class {
            RequestClass::Transfer => (&self.transfer_sender, TRANSFER_QUEUE_CAPACITY),
            RequestClass::Exec => (&self.exec_sender, EXEC_QUEUE_CAPACITY),
            RequestClass::Control => (&self.control_sender, CONTROL_QUEUE_CAPACITY),
        };
        match sender.try_send(RequestJob {
            stream,
            workspace_key: None,
        }) {
            Ok(()) => Ok(()),
            Err(mpsc::TrySendError::Full(job)) => {
                reject_busy(job.stream, class, capacity);
                Ok(())
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                Err(anyhow::anyhow!("daemon {class:?} queue is closed"))
            }
        }
    }

    /// Stop taking work and join the workers, giving up after the grace period.
    ///
    /// A worker in the middle of a request cannot be interrupted safely, so the
    /// only bound available is how long shutdown waits before leaving the
    /// remaining workers behind. Returns the number of workers still running,
    /// which is the number of operations the caller has to report as incomplete.
    pub(super) fn finish_within(self, grace: Duration) -> usize {
        let Self {
            control_sender,
            transfer_sender,
            exec_sender,
            exec_in_flight: _exec_in_flight,
            exec_per_workspace: _exec_per_workspace,
            workers,
        } = self;
        drop(control_sender);
        drop(transfer_sender);
        drop(exec_sender);

        let deadline = std::time::Instant::now() + grace;
        loop {
            if workers.iter().all(|worker| worker.is_finished()) {
                break;
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }

        let mut unfinished = 0;
        for worker in workers {
            if worker.is_finished() {
                let _ = worker.join();
            } else {
                // The process is about to exit, so the handle is dropped rather
                // than waited on. Its operation is named in the shutdown report.
                unfinished += 1;
            }
        }
        unfinished
    }
}

fn spawn_workers(
    receiver: Arc<Mutex<Receiver<RequestJob>>>,
    count: usize,
    class: &'static str,
    config: &DaemonConfig,
    shutdown: &Arc<AtomicBool>,
    services: &super::services::DaemonServices,
    exec_in_flight: Arc<Mutex<std::collections::HashMap<String, usize>>>,
) -> Result<Vec<thread::JoinHandle<()>>> {
    let mut workers = Vec::with_capacity(count);
    for worker_id in 0..count {
        let receiver = Arc::clone(&receiver);
        let config = config.clone();
        let shutdown = Arc::clone(shutdown);
        let services = services.clone();
        let exec_in_flight = Arc::clone(&exec_in_flight);
        let worker = thread::Builder::new()
            .name(format!("enclave-daemon-{class}-worker-{worker_id}"))
            .spawn(move || loop {
                let job = match receiver.lock() {
                    Ok(receiver) => receiver.recv(),
                    Err(_) => return,
                };
                let Ok(job) = job else {
                    return;
                };
                // The slot is released whatever the request does, including a
                // panic, so a failed command cannot permanently consume its
                // workspace's share.
                let outcome =
                    run_request(|| handle_client(job.stream, &config, &shutdown, &services));
                if let Some(key) = job.workspace_key.as_deref() {
                    release_exec_slot(&exec_in_flight, key);
                }
                match outcome {
                    RequestOutcome::Finished => {}
                    RequestOutcome::Failed(error) => {
                        tracing::warn!(worker_id, class, "sandbox daemon request error: {error:#}")
                    }
                    RequestOutcome::Panicked(message) => tracing::error!(
                        worker_id,
                        class,
                        "sandbox daemon request panicked: {message}"
                    ),
                }
            })
            .with_context(|| format!("failed to spawn daemon {class} worker {worker_id}"))?;
        workers.push(worker);
    }
    Ok(workers)
}

/// Give one exec slot back to a workspace.
fn release_exec_slot(
    in_flight: &Arc<Mutex<std::collections::HashMap<String, usize>>>,
    workspace_key: &str,
) {
    let mut in_flight = in_flight.lock().unwrap_or_else(|error| error.into_inner());
    if let Some(count) = in_flight.get_mut(workspace_key) {
        *count = count.saturating_sub(1);
        if *count == 0 {
            in_flight.remove(workspace_key);
        }
    }
}

/// What one request did, once a panic cannot escape it.
mod request;

use request::{classify_stream, peek_workspace_key, run_request, RequestClass, RequestOutcome};

/// Tell a client its request was refused because the pool is saturated.
///
/// The response uses the same wire shape as a dispatched request, so the client's
/// normal error path reports it, and it names the pool so an operator can tell
/// which one is full without reading the daemon's log.
fn reject_busy(mut stream: UnixStream, class: RequestClass, capacity: usize) {
    let response = crate::protocol::Response {
        ok: false,
        result: None,
        error: Some(format!(
            "daemon {class:?} queue is full ({capacity} pending); retry the request"
        )),
        code: Some(crate::error::ErrorCode::RateLimited),
        operation_id: uuid::Uuid::new_v4().to_string(),
    };
    if let Ok(payload) = serde_json::to_vec(&response) {
        let _ = stream.write_all(&payload);
        let _ = stream.write_all(b"\n");
        let _ = stream.flush();
    }
}

#[cfg(test)]
#[path = "../../../tests/src/daemon/workers.rs"]
mod tests;
