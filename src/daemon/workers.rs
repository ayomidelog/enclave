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
#[derive(Debug, PartialEq, Eq)]
enum RequestOutcome {
    Finished,
    Failed(String),
    Panicked(String),
}

/// Serve one request, turning a panic into a report rather than an unwind.
///
/// A panic in one request must not cost the daemon a worker. The control pool is
/// six threads, so six panics would leave the daemon running and accepting
/// connections while nothing answers a lifecycle request. Catching here turns
/// that into one failed request and a logged panic.
fn run_request<F>(operation: F) -> RequestOutcome
where
    F: FnOnce() -> Result<()>,
{
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation)) {
        Ok(Ok(())) => RequestOutcome::Finished,
        Ok(Err(error)) => RequestOutcome::Failed(format!("{error:#}")),
        Err(payload) => RequestOutcome::Panicked(panic_message(&payload)),
    }
}

/// The text of a caught panic payload.
///
/// A panic payload is whatever was passed to `panic!`, which is a `&str` or a
/// `String` for every panic this codebase raises. Anything else is reported by
/// its type rather than dropped, so a panic is never silent.
fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_string();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    "<non-string panic payload>".to_string()
}

fn stream_is_transfer(stream: &UnixStream) -> bool {
    let mut readiness = libc::pollfd {
        fd: stream.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let poll_result = unsafe { libc::poll(&mut readiness, 1, 10) };
    if poll_result <= 0 || readiness.revents & libc::POLLIN == 0 {
        return false;
    }
    let mut buffer = [0u8; 4096];
    let size = unsafe {
        libc::recv(
            stream.as_raw_fd(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            libc::MSG_PEEK | libc::MSG_DONTWAIT,
        )
    };
    if size <= 0 {
        return false;
    }
    String::from_utf8_lossy(&buffer[..size as usize]).contains("\"workspace.cp\"")
}

#[cfg(test)]
mod tests {
    use super::{configured_worker_count, run_request, stream_is_transfer, RequestOutcome};
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;

    #[test]
    fn classifies_workspace_copy_without_consuming_request() {
        let (mut writer, reader) = UnixStream::pair().unwrap();
        writer
            .write_all(br#"{"action":"workspace.cp","params":{}}\n"#)
            .unwrap();
        assert!(stream_is_transfer(&reader));
        let mut buffer = [0u8; 128];
        let size = unsafe {
            libc::recv(
                reader.as_raw_fd(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                libc::MSG_PEEK,
            )
        };
        assert!(size > 0);
        assert!(String::from_utf8_lossy(&buffer[..size as usize]).contains("workspace.cp"));
    }

    #[test]
    fn classifies_control_request_separately() {
        let (mut writer, reader) = UnixStream::pair().unwrap();
        writer
            .write_all(br#"{"action":"workspace.list","params":{}}\n"#)
            .unwrap();
        assert!(!stream_is_transfer(&reader));
    }

    #[test]
    fn worker_count_rejects_unbounded_and_invalid_values() {
        unsafe { std::env::set_var("ENCLAVE_TEST_WORKER_COUNT", "0") };
        assert_eq!(configured_worker_count("ENCLAVE_TEST_WORKER_COUNT", 6), 6);
        unsafe { std::env::set_var("ENCLAVE_TEST_WORKER_COUNT", "65") };
        assert_eq!(configured_worker_count("ENCLAVE_TEST_WORKER_COUNT", 6), 6);
        unsafe { std::env::set_var("ENCLAVE_TEST_WORKER_COUNT", "3") };
        assert_eq!(configured_worker_count("ENCLAVE_TEST_WORKER_COUNT", 6), 3);
        unsafe { std::env::remove_var("ENCLAVE_TEST_WORKER_COUNT") };
    }

    #[test]
    fn a_panicking_request_is_reported_and_does_not_escape() {
        // The daemon's control pool is six threads. If a panic escaped here the
        // thread would die, and six panics would leave the daemon running and
        // accepting connections while nothing answered a lifecycle request.
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let outcome = run_request(|| panic!("simulated request panic"));
        std::panic::set_hook(previous);
        assert_eq!(
            outcome,
            RequestOutcome::Panicked("simulated request panic".to_string())
        );
    }

    #[test]
    fn a_request_error_and_a_success_are_reported_separately() {
        assert_eq!(run_request(|| Ok(())), RequestOutcome::Finished);
        let outcome = run_request(|| Err(anyhow::anyhow!("disk full")));
        assert_eq!(outcome, RequestOutcome::Failed("disk full".to_string()));
    }

    #[test]
    fn a_worker_keeps_serving_after_a_panicking_request() {
        // The loop the worker runs is this sequence, so the second request after a
        // panic is what proves the worker is still usable.
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let first = run_request(|| panic!("boom"));
        let second = run_request(|| Ok(()));
        std::panic::set_hook(previous);
        assert!(matches!(first, RequestOutcome::Panicked(_)));
        assert_eq!(second, RequestOutcome::Finished);
    }
}
