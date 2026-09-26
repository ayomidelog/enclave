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
const CONTROL_QUEUE_CAPACITY: usize = 128;
const TRANSFER_QUEUE_CAPACITY: usize = 8;
const MAX_WORKER_COUNT: usize = 64;

struct RequestJob {
    stream: UnixStream,
}

pub(super) struct RequestWorkerPool {
    control_sender: SyncSender<RequestJob>,
    transfer_sender: SyncSender<RequestJob>,
    workers: Vec<thread::JoinHandle<()>>,
}

impl RequestWorkerPool {
    pub(super) fn new(
        config: &DaemonConfig,
        shutdown: &Arc<AtomicBool>,
        services: &super::services::DaemonServices,
    ) -> Result<Self> {
        let control_count =
            configured_worker_count("ENCLAVE_CONTROL_WORKERS", CONTROL_WORKER_COUNT);
        let transfer_count =
            configured_worker_count("ENCLAVE_TRANSFER_WORKERS", TRANSFER_WORKER_COUNT);
        let (control_sender, control_receiver) = mpsc::sync_channel(CONTROL_QUEUE_CAPACITY);
        let (transfer_sender, transfer_receiver) = mpsc::sync_channel(TRANSFER_QUEUE_CAPACITY);
        let control_receiver = Arc::new(Mutex::new(control_receiver));
        let transfer_receiver = Arc::new(Mutex::new(transfer_receiver));
        let mut workers = Vec::with_capacity(control_count + transfer_count);
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
        Ok(Self {
            control_sender,
            transfer_sender,
            workers,
        })
    }

    pub(super) fn submit(&self, stream: UnixStream) -> Result<()> {
        let is_transfer = stream_is_transfer(&stream);
        let sender = if is_transfer {
            &self.transfer_sender
        } else {
            &self.control_sender
        };
        sender.send(RequestJob { stream }).context(if is_transfer {
            "daemon transfer queue is closed or full"
        } else {
            "daemon control queue is closed or full"
        })
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
            workers,
        } = self;
        drop(control_sender);
        drop(transfer_sender);

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

fn configured_worker_count(variable: &str, default: usize) -> usize {
    std::env::var(variable)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| (1..=MAX_WORKER_COUNT).contains(value))
        .unwrap_or(default)
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
                match run_request(|| handle_client(job.stream, &config, &shutdown, &services)) {
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

/// What one request did, once a panic cannot escape it.
mod request;

use request::{run_request, stream_is_transfer, RequestOutcome};

#[cfg(test)]
#[path = "../../../tests/src/daemon/workers.rs"]
mod tests;
