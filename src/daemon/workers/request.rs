//! What one request produced, so the worker can report it and keep going.
//!
//! The pool exists so a request cannot take the daemon down with it. An error is
//! reported to the client and the worker keeps its place; a panic is caught at
//! the same boundary, because six panics would otherwise leave the daemon
//! accepting connections while nothing answered a lifecycle request.

use super::*;

/// The request-line prefix the dispatcher is willing to inspect.
///
/// The daemon's request reader accepts 64 KiB, so the peek buffer matches that
/// bound. A request larger than this is left for the worker to reject.
const MAX_PEEK_BYTES: usize = 64 * 1024;

/// How long the dispatcher waits for a complete request line before classifying
/// the bytes it has. Clients send the line in one write, so this only covers a
/// request split across two writes; it must stay short because it runs on the
/// accept loop.
const PEEK_WAIT: std::time::Duration = std::time::Duration::from_millis(10);

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

/// What the dispatcher can tell about a request without consuming it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PeekedRequest {
    pub(super) class: RequestClass,
    pub(super) workspace_key: Option<String>,
}

impl Default for PeekedRequest {
    fn default() -> Self {
        Self {
            class: RequestClass::Control,
            workspace_key: None,
        }
    }
}

/// Peek at the request line and pick its pool, without consuming the bytes.
///
/// The peek is best-effort: a request whose bytes have not arrived yet is treated
/// as control, which is the only class that can be answered without a payload
/// that large. The line is left on the socket for the worker that serves it.
#[cfg(test)]
pub(super) fn classify_stream(stream: &UnixStream) -> RequestClass {
    peek_request(stream).class
}

/// Peek the workspace an exec request targets, without consuming the request.
///
/// The pool uses this to give each workspace a bounded share of exec workers, so a
/// workspace issuing several slow commands cannot take the whole pool while others
/// starve. A request whose body has not arrived yet has no key: it is admitted
/// normally and takes its chances like any other, because blocking the accept loop
/// to wait for the body would be worse than the unfairness it avoids.
#[cfg(test)]
pub(super) fn peek_workspace_key(stream: &UnixStream) -> Option<String> {
    peek_request(stream).workspace_key
}

/// Inspect a request without consuming it, classifying from the parsed action.
///
/// Matching action names as substrings let a command argument or a parameter
/// value move an unrelated request into the exec pool, and a request split across
/// writes could fall back to the control pool while it was really an exec. This
/// waits briefly for the newline-terminated line and parses it as JSON, so the
/// decision comes from the `action` field rather than from text anywhere in the
/// payload.
pub(super) fn peek_request(stream: &UnixStream) -> PeekedRequest {
    let Some(line) = peek_request_line(stream) else {
        return PeekedRequest::default();
    };
    let Ok(request) = serde_json::from_slice::<crate::protocol::Request>(&line) else {
        return PeekedRequest::default();
    };
    let class = match request.action.as_str() {
        "workspace.cp" => RequestClass::Transfer,
        // `workspace.exec` and `sandbox.exec_setup` both run a caller-supplied
        // program, so neither may occupy a control worker.
        "workspace.exec" | "sandbox.exec_setup" => RequestClass::Exec,
        _ => RequestClass::Control,
    };
    let workspace_key = (class == RequestClass::Exec)
        .then(|| workspace_key_from_params(&request.params))
        .flatten();
    PeekedRequest {
        class,
        workspace_key,
    }
}

fn workspace_key_from_params(params: &serde_json::Value) -> Option<String> {
    ["workspace", "workspace_id", "name"]
        .into_iter()
        .find_map(|field| params.get(field).and_then(serde_json::Value::as_str))
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Return the newline-terminated request line without consuming socket bytes.
fn peek_request_line(stream: &UnixStream) -> Option<Vec<u8>> {
    let deadline = std::time::Instant::now() + PEEK_WAIT;
    let mut buffer = vec![0u8; MAX_PEEK_BYTES];
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let timeout_ms = remaining.as_millis().min(i32::MAX as u128) as libc::c_int;
        let mut readiness = libc::pollfd {
            fd: stream.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let poll_result = unsafe { libc::poll(&mut readiness, 1, timeout_ms) };
        if poll_result <= 0 || readiness.revents & libc::POLLIN == 0 {
            return None;
        }
        let size = unsafe {
            libc::recv(
                stream.as_raw_fd(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                libc::MSG_PEEK | libc::MSG_DONTWAIT,
            )
        };
        if size <= 0 {
            return None;
        }
        let bytes = &buffer[..size as usize];
        if let Some(end) = bytes.iter().position(|byte| *byte == b'\n') {
            return Some(bytes[..end].to_vec());
        }
        if remaining.is_zero() {
            return None;
        }
    }
}
