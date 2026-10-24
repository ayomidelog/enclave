//! Sending one command to a live helper and reading its response.

use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::workspace::types::WorkspaceMetadata;

use super::{PersistentHelper, MAX_HELPER_OUTPUT_BYTES};

/// The largest response one helper can produce.
///
/// The helper serializes stdout and stderr as JSON strings, and a JSON escape
/// can expand one byte to six. A response larger than this is a broken helper,
/// not a command result, and reading it without a bound would let that helper
/// consume the daemon's memory.
const MAX_HELPER_RESPONSE_BYTES: usize = 2 * MAX_HELPER_OUTPUT_BYTES * 6 + 64 * 1024;

/// The helper stopped answering and must be killed and evicted.
#[derive(Debug)]
pub(super) struct HelperTimeout {
    pub(super) message: String,
}

impl std::fmt::Display for HelperTimeout {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for HelperTimeout {}

/// The helper closed its connection without returning a command result.
///
/// This is a failure of the helper, not of the command. The caller evicts the
/// helper and starts a fresh one before retrying the command once.
#[derive(Debug)]
pub(super) struct HelperClosed {
    pub(super) message: String,
}

impl std::fmt::Display for HelperClosed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for HelperClosed {}

impl HelperTimeout {
    pub(super) fn is_in(error: &anyhow::Error) -> bool {
        error.chain().any(|cause| cause.is::<Self>())
    }
}

impl HelperClosed {
    pub(super) fn is_in(error: &anyhow::Error) -> bool {
        error.chain().any(|cause| cause.is::<Self>())
    }
}

#[derive(Debug, Serialize)]
struct CommandRequest<'a> {
    auth_token: &'a str,
    runtime_pid: u32,
    runtime_starttime_ticks: u64,
    sandbox_id: &'a str,
    workspace_id: &'a str,
    cwd: &'a str,
    command: &'a [String],
}

#[derive(Debug, Deserialize)]
struct CommandResponse {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

#[derive(Debug)]
pub(crate) struct PersistentCommandOutput {
    pub(crate) exit_code: i32,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    /// The host pid of the helper that supervised the command.
    pub(crate) helper_pid: Option<u32>,
}

pub(super) fn send_command(
    helper: &mut PersistentHelper,
    runtime_pid: u32,
    runtime_starttime_ticks: u64,
    workspace: &WorkspaceMetadata,
    cwd: &str,
    command: &[String],
    client_stream: Option<&UnixStream>,
) -> Result<PersistentCommandOutput> {
    let started = Instant::now();
    // The helper creates its socket just before it is ready, so a connect that
    // arrives first is retried rather than treated as a dead helper.
    let mut stream = loop {
        match UnixStream::connect(&helper.socket) {
            Ok(stream) => break stream,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                ) && started.elapsed() < Duration::from_secs(1) =>
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => {
                return Err(helper_failure(
                    workspace,
                    format!(
                        "failed to connect to helper at {}: {error}",
                        helper.socket.display()
                    ),
                ))
            }
        }
    };
    let request = CommandRequest {
        auth_token: &helper.auth_token,
        runtime_pid,
        runtime_starttime_ticks,
        sandbox_id: &workspace.sandbox_id,
        workspace_id: &workspace.id,
        cwd,
        command,
    };
    serde_json::to_writer(&mut stream, &request)
        .map_err(|error| helper_failure(workspace, format!("failed to encode command: {error}")))?;
    stream.write_all(b"\n").map_err(|error| {
        helper_failure(workspace, format!("failed to terminate command: {error}"))
    })?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .map_err(|error| helper_failure(workspace, format!("failed to finish command: {error}")))?;
    // The helper answers only after the command's own child exits, so this bound is
    // also the longest one command may run. It is generous by default and only
    // exists so a command that never exits cannot pin this thread and its exec
    // worker forever.
    let deadline = crate::deadlines::helper_response();
    stream.set_nonblocking(true).map_err(|error| {
        helper_failure(
            workspace,
            format!("failed to make helper response nonblocking: {error}"),
        )
    })?;
    let mut response_line = String::new();
    read_response(
        &mut stream,
        &mut response_line,
        workspace,
        deadline.value,
        deadline.variable,
        client_stream,
    )?;
    if response_line.is_empty() {
        return Err(anyhow::Error::new(HelperClosed {
            message: format!(
                "helper for workspace {} in sandbox {} closed the connection without a result",
                workspace.id, workspace.sandbox_id
            ),
        }));
    }
    let response: CommandResponse = serde_json::from_str(&response_line).map_err(|error| {
        helper_failure(
            workspace,
            format!("failed to decode command response: {error}"),
        )
    })?;
    Ok(PersistentCommandOutput {
        exit_code: response.exit_code,
        stdout: response.stdout,
        stderr: response.stderr,
        helper_pid: None,
    })
}

/// Wait for the helper's one-line response while watching the client connection.
///
/// The helper owns the command and can only be interrupted through its socket, so
/// the caller's disconnect has to be observed here rather than in the transport
/// reader. A helper that answers at the same moment the client leaves is still a
/// completed command; the connection is checked before cancelling.
fn read_response(
    stream: &mut UnixStream,
    response_line: &mut String,
    workspace: &WorkspaceMetadata,
    timeout: Duration,
    variable: &str,
    client_stream: Option<&UnixStream>,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    let mut response = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(anyhow::Error::new(HelperTimeout {
                message: format!(
                    "helper for workspace {} in sandbox {} did not answer within {:?} (raise {variable} to allow longer commands)",
                    workspace.id, workspace.sandbox_id, timeout
                ),
            }));
        }
        let mut descriptors = [
            libc::pollfd {
                fd: stream.as_raw_fd(),
                events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
                revents: 0,
            },
            libc::pollfd {
                fd: client_stream.map_or(-1, AsRawFd::as_raw_fd),
                events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
                revents: 0,
            },
        ];
        let count = if client_stream.is_some() { 2 } else { 1 };
        let wait_ms = remaining.as_millis().clamp(1, 100) as libc::c_int;
        let ready = unsafe { libc::poll(descriptors.as_mut_ptr(), count, wait_ms) };
        if ready < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(helper_failure(
                workspace,
                format!("failed while waiting for helper response: {error}"),
            ));
        }
        if descriptors[0].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
            // Read only bytes that are already available. The socket is
            // nonblocking so a helper that writes half a response and stalls
            // cannot hold this worker until the client deadline.
            let mut chunk = [0_u8; 64 * 1024];
            loop {
                match stream.read(&mut chunk) {
                    Ok(0) => {
                        return Err(helper_failure(
                            workspace,
                            "helper closed the connection before a complete response".to_string(),
                        ))
                    }
                    Ok(read) => {
                        response.extend_from_slice(&chunk[..read]);
                        if response.len() > MAX_HELPER_RESPONSE_BYTES {
                            return Err(helper_failure(
                                workspace,
                                format!(
                                    "helper response exceeded {} bytes",
                                    MAX_HELPER_RESPONSE_BYTES
                                ),
                            ));
                        }
                        if let Some(end) = response.iter().position(|byte| *byte == b'\n') {
                            *response_line =
                                String::from_utf8(response[..end].to_vec()).map_err(|error| {
                                    helper_failure(
                                        workspace,
                                        format!("helper response was not UTF-8: {error}"),
                                    )
                                })?;
                            return Ok(());
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => {
                        return Err(helper_failure(
                            workspace,
                            format!("failed to read helper response: {error}"),
                        ))
                    }
                }
            }
        }
        if client_stream.is_some_and(client_disconnected) {
            return Err(anyhow::Error::new(ClientDisconnected {
                message: format!(
                    "client disconnected; cancelled the command in workspace {} in sandbox {}",
                    workspace.id, workspace.sandbox_id
                ),
            }));
        }
    }
}

/// A helper transport failure that means the cached helper must be replaced.
fn helper_failure(workspace: &WorkspaceMetadata, detail: String) -> anyhow::Error {
    anyhow::Error::new(HelperClosed {
        message: format!(
            "helper for workspace {} in sandbox {} failed: {detail}",
            workspace.id, workspace.sandbox_id
        ),
    })
}

/// Whether the daemon's client has gone away.
///
/// A Unix socket may report readable on EOF without setting `POLLHUP`, so a
/// `POLLIN` is confirmed with a nonblocking peek. Any unexpected byte is left
/// alone; only EOF means the caller has disconnected.
fn client_disconnected(stream: &UnixStream) -> bool {
    let mut descriptor = libc::pollfd {
        fd: stream.as_raw_fd(),
        events: libc::POLLIN | libc::POLLRDHUP | libc::POLLHUP | libc::POLLERR,
        revents: 0,
    };
    let result = unsafe { libc::poll(&mut descriptor, 1, 0) };
    if result <= 0 {
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

/// A client left before its command returned.
#[derive(Debug)]
pub(super) struct ClientDisconnected {
    pub(super) message: String,
}

impl std::fmt::Display for ClientDisconnected {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ClientDisconnected {}

impl ClientDisconnected {
    pub(super) fn is_in(error: &anyhow::Error) -> bool {
        error.chain().any(|cause| cause.is::<Self>())
    }
}

#[cfg(test)]
#[path = "../../../../tests/src/workspace/session/persistent_transport.rs"]
mod tests;
