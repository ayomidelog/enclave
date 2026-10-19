//! What one request produced, so the worker can report it and keep going.
//!
//! The pool exists so a request cannot take the daemon down with it. An error is
//! reported to the client and the worker keeps its place; a panic is caught at
//! the same boundary, because six panics would otherwise leave the daemon
//! accepting connections while nothing answered a lifecycle request.

use super::*;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum RequestOutcome {
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
pub(super) fn run_request<F>(operation: F) -> RequestOutcome
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

/// Which pool serves a request, decided from the first bytes on the socket.
///
/// The split exists because the pools have different lifetimes. A lifecycle call
/// finishes in milliseconds, while `workspace exec` runs for as long as the
/// caller's command does and `workspace cp` streams a whole directory. When they
/// shared one pool, six long-running commands occupied every control worker and
/// the daemon stopped answering `ping` while it was still perfectly healthy: the
/// CLI called that a dead daemon, and the gateway reaped a daemon that had done
/// nothing wrong. Each pool now saturates on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RequestClass {
    /// Fast lifecycle, status, and policy calls.
    Control,
    /// Commands that run for as long as the caller's program does.
    Exec,
    /// Whole-tree copies, which stream large payloads in both directions.
    Transfer,
}

/// Peek at the request line and pick its pool, without consuming the bytes.
///
/// The peek is best-effort: a request whose bytes have not arrived yet is treated
/// as control, which is the only class that can be answered without a payload
/// that large. The line is left on the socket for the worker that serves it.
pub(super) fn classify_stream(stream: &UnixStream) -> RequestClass {
    let mut readiness = libc::pollfd {
        fd: stream.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let poll_result = unsafe { libc::poll(&mut readiness, 1, 10) };
    if poll_result <= 0 || readiness.revents & libc::POLLIN == 0 {
        return RequestClass::Control;
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
        return RequestClass::Control;
    }
    let head = String::from_utf8_lossy(&buffer[..size as usize]);
    if head.contains("\"workspace.cp\"") {
        return RequestClass::Transfer;
    }
    // `workspace.exec` and `sandbox.exec_setup` both run a caller-supplied
    // program, so neither may occupy a control worker.
    if head.contains("\"workspace.exec\"") || head.contains("\"sandbox.exec_setup\"") {
        return RequestClass::Exec;
    }
    RequestClass::Control
}
