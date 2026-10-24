use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Condvar, Mutex};
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

struct RequestJob {
    stream: UnixStream,
    /// The workspace this request targets, when it is an exec. The queue only
    /// releases a job to a worker while that workspace is below its share.
    workspace_key: Option<String>,
}

/// The exec pool's queue and its per-workspace admission state.
///
/// A workspace may have at most `per_workspace` jobs in flight. The default is
/// effectively unlimited, leaving the shared exec worker count as the capacity
/// bound. An operator can configure a smaller share; jobs over it then wait in a
/// per-workspace queue instead of entering the shared worker queue.
struct ExecQueue {
    state: Mutex<ExecQueueState>,
    ready: Condvar,
    per_workspace: Option<usize>,
}

#[derive(Default)]
struct ExecQueueState {
    ready: VecDeque<RequestJob>,
    pending: HashMap<String, VecDeque<UnixStream>>,
    pending_count: usize,
    in_flight: HashMap<String, usize>,
    closed: bool,
}

enum ExecEnqueueError {
    Full(RequestJob),
    Closed,
}

impl ExecQueue {
    fn new(per_workspace: Option<usize>) -> Self {
        Self {
            state: Mutex::new(ExecQueueState::default()),
            ready: Condvar::new(),
            per_workspace,
        }
    }

    fn enqueue(&self, job: RequestJob, capacity: usize) -> Result<(), ExecEnqueueError> {
        let mut state = self.lock();
        discard_disconnected(&mut state);
        if state.closed {
            return Err(ExecEnqueueError::Closed);
        }
        if state.ready.len().saturating_add(state.pending_count) >= capacity {
            return Err(ExecEnqueueError::Full(job));
        }
        match job.workspace_key.as_deref() {
            Some(key)
                if self.per_workspace.is_some_and(|limit| {
                    state.in_flight.get(key).copied().unwrap_or(0) >= limit
                }) || state
                    .pending
                    .get(key)
                    .is_some_and(|pending| !pending.is_empty()) =>
            {
                state
                    .pending
                    .entry(key.to_string())
                    .or_default()
                    .push_back(job.stream);
                state.pending_count += 1;
            }
            _ => {
                if let Some(key) = job.workspace_key.as_deref() {
                    *state.in_flight.entry(key.to_string()).or_insert(0) += 1;
                }
                state.ready.push_back(job);
            }
        }
        drop(state);
        self.ready.notify_all();
        Ok(())
    }

    /// Wait for the next admitted request.
    fn next(&self) -> Option<RequestJob> {
        let mut state = self.lock();
        loop {
            if state.closed {
                return None;
            }
            if let Some(job) = state.ready.pop_front() {
                return Some(job);
            }
            state = self
                .ready
                .wait(state)
                .unwrap_or_else(|error| error.into_inner());
        }
    }

    fn finish(&self, workspace_key: Option<&str>) {
        let mut state = self.lock();
        discard_disconnected(&mut state);
        if let Some(key) = workspace_key {
            if let Some(count) = state.in_flight.get_mut(key) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    state.in_flight.remove(key);
                }
            }
            let has_room = self
                .per_workspace
                .is_none_or(|limit| state.in_flight.get(key).copied().unwrap_or(0) < limit);
            if has_room {
                if let Some(stream) = state.pending.get_mut(key).and_then(VecDeque::pop_front) {
                    state.pending_count = state.pending_count.saturating_sub(1);
                    *state.in_flight.entry(key.to_string()).or_insert(0) += 1;
                    state.ready.push_back(RequestJob {
                        stream,
                        workspace_key: Some(key.to_string()),
                    });
                }
            }
        }
        drop(state);
        self.ready.notify_all();
    }

    fn close(&self) {
        let mut state = self.lock();
        state.closed = true;
        state.ready.clear();
        state.pending.clear();
        state.pending_count = 0;
        drop(state);
        self.ready.notify_all();
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ExecQueueState> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }
}

/// Drop queued requests whose clients have gone away.
///
/// A queued request has not reached a worker yet, so it has no other place that
/// notices the caller leaving. Keeping its socket would count an abandoned job
/// against the bounded queue and could make a healthy daemon report itself full.
fn discard_disconnected(state: &mut ExecQueueState) {
    state.ready.retain(|job| !client_disconnected(&job.stream));
    for pending in state.pending.values_mut() {
        pending.retain(|stream| !client_disconnected(stream));
    }
    state.pending.retain(|_, pending| !pending.is_empty());
    state.pending_count = state.pending.values().map(VecDeque::len).sum();
}

/// Whether a queued client has closed its request connection.
fn client_disconnected(stream: &UnixStream) -> bool {
    let mut descriptor = libc::pollfd {
        fd: stream.as_raw_fd(),
        events: libc::POLLIN | libc::POLLRDHUP | libc::POLLHUP | libc::POLLERR,
        revents: 0,
    };
    let ready = unsafe { libc::poll(&mut descriptor, 1, 0) };
    if ready <= 0 {
        return false;
    }
    if descriptor.revents & (libc::POLLRDHUP | libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0
    {
        return true;
    }
    if descriptor.revents & libc::POLLIN == 0 {
        return false;
    }
    let mut byte = 0_u8;
    let read = unsafe {
        libc::recv(
            stream.as_raw_fd(),
            (&mut byte as *mut u8).cast(),
            1,
            libc::MSG_PEEK | libc::MSG_DONTWAIT,
        )
    };
    read == 0
}

pub(super) struct RequestWorkerPool {
    control_sender: SyncSender<RequestJob>,
    transfer_sender: SyncSender<RequestJob>,
    exec_queue: Arc<ExecQueue>,
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
        let control_receiver = Arc::new(Mutex::new(control_receiver));
        let transfer_receiver = Arc::new(Mutex::new(transfer_receiver));
        // A workspace has no artificial share by default. The exec worker pool is
        // the daemon-wide capacity bound, just as host resources bound an ordinary
        // machine. Operators can opt into a smaller per-workspace share.
        let exec_per_workspace = crate::workspace::max_concurrent_commands_per_workspace();
        let exec_queue = Arc::new(ExecQueue::new(exec_per_workspace));
        let mut workers = Vec::with_capacity(control_count + transfer_count + exec_count);
        workers.extend(spawn_workers(
            control_receiver,
            control_count,
            "control",
            config,
            shutdown,
            services,
        )?);
        workers.extend(spawn_workers(
            transfer_receiver,
            transfer_count,
            "transfer",
            config,
            shutdown,
            services,
        )?);
        workers.extend(spawn_exec_workers(
            Arc::clone(&exec_queue),
            exec_count,
            config,
            shutdown,
            services,
        )?);
        Ok(Self {
            control_sender,
            transfer_sender,
            exec_queue,
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
        let peeked = peek_request(&stream);
        let class = peeked.class;
        // Exec requests carry the workspace they target, so a workspace already at
        // its share waits in the queue rather than taking a worker another
        // workspace needs.
        let workspace_key = (class == RequestClass::Exec)
            .then_some(peeked.workspace_key)
            .flatten();
        let job = RequestJob {
            stream,
            workspace_key,
        };
        if class == RequestClass::Exec {
            return match self.exec_queue.enqueue(job, EXEC_QUEUE_CAPACITY) {
                Ok(()) => Ok(()),
                Err(ExecEnqueueError::Full(job)) => {
                    tracing::warn!(
                        class = ?class,
                        capacity = EXEC_QUEUE_CAPACITY,
                        "daemon request queue is full; rejecting the connection"
                    );
                    reject_busy(job.stream, class, EXEC_QUEUE_CAPACITY);
                    Ok(())
                }
                Err(ExecEnqueueError::Closed) => {
                    Err(anyhow::anyhow!("daemon {class:?} queue is closed"))
                }
            };
        }
        let (sender, capacity) = match class {
            RequestClass::Transfer => (&self.transfer_sender, TRANSFER_QUEUE_CAPACITY),
            RequestClass::Control => (&self.control_sender, CONTROL_QUEUE_CAPACITY),
            RequestClass::Exec => unreachable!("exec requests return through the exec queue"),
        };
        match sender.try_send(job) {
            Ok(()) => Ok(()),
            Err(mpsc::TrySendError::Full(job)) => {
                tracing::warn!(
                    class = ?class,
                    capacity,
                    "daemon request queue is full; rejecting the connection"
                );
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
            exec_queue,
            workers,
        } = self;
        drop(control_sender);
        drop(transfer_sender);
        exec_queue.close();

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
) -> Result<Vec<thread::JoinHandle<()>>> {
    let mut workers = Vec::with_capacity(count);
    for worker_id in 0..count {
        let receiver = Arc::clone(&receiver);
        let config = config.clone();
        let shutdown = Arc::clone(shutdown);
        let services = services.clone();
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
                let outcome =
                    run_request(|| handle_client(job.stream, &config, &shutdown, &services));
                match outcome {
                    RequestOutcome::Finished => {}
                    // `handle_client` logs the failure with its action and target.
                    // This boundary only keeps the worker alive.
                    RequestOutcome::Failed(_) => {}
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

fn spawn_exec_workers(
    queue: Arc<ExecQueue>,
    count: usize,
    config: &DaemonConfig,
    shutdown: &Arc<AtomicBool>,
    services: &super::services::DaemonServices,
) -> Result<Vec<thread::JoinHandle<()>>> {
    let mut workers = Vec::with_capacity(count);
    for worker_id in 0..count {
        let queue = Arc::clone(&queue);
        let config = config.clone();
        let shutdown = Arc::clone(shutdown);
        let services = services.clone();
        let worker = thread::Builder::new()
            .name(format!("enclave-daemon-exec-worker-{worker_id}"))
            .spawn(move || loop {
                let Some(job) = queue.next() else {
                    return;
                };
                let workspace_key = job.workspace_key.clone();
                let outcome =
                    run_request(|| handle_client(job.stream, &config, &shutdown, &services));
                // Released after every outcome, including a panic, so a failed
                // command cannot permanently consume its workspace's share.
                queue.finish(workspace_key.as_deref());
                match outcome {
                    RequestOutcome::Finished => {}
                    // `handle_client` has already logged the failed request with
                    // its action and target, so repeating it here would attribute
                    // one failure twice and still not name the workspace.
                    RequestOutcome::Failed(_) => {}
                    RequestOutcome::Panicked(message) => tracing::error!(
                        worker_id,
                        class = "exec",
                        "sandbox daemon request panicked: {message}"
                    ),
                }
            })
            .with_context(|| format!("failed to spawn daemon exec worker {worker_id}"))?;
        workers.push(worker);
    }
    Ok(workers)
}

/// What one request did, once a panic cannot escape it.
mod request;

use request::{peek_request, run_request, RequestClass, RequestOutcome};

#[cfg(test)]
use request::{classify_stream, peek_workspace_key};

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
