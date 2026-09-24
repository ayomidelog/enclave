use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};

const MAX_RESPONSE_BYTES: usize = 512 * 1024;
const CLIENT_IO_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, Deserialize)]
struct ResponsePayload {
    ok: bool,
    result: Option<Value>,
    error: Option<String>,
    #[serde(default)]
    operation_id: Option<String>,
}

thread_local! {
    /// The operation id of the last response this thread read.
    ///
    /// The daemon names one operation per request and reports it in the
    /// response. A command prints it after a lifecycle call, and a CLI process
    /// serves one command on one thread, so remembering it here keeps every
    /// existing call site unchanged.
    static LAST_OPERATION_ID: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

/// The operation id the daemon reported for the last response on this thread.
pub fn last_operation_id() -> Option<String> {
    LAST_OPERATION_ID.with(|id| id.borrow().clone())
}

/// A request that never reached a daemon.
///
/// Callers that would otherwise start a daemon need to tell "nothing is
/// listening" apart from "the daemon answered with an error", so the distinction
/// is a type rather than a substring of the message.
#[derive(Debug)]
pub enum DaemonUnreachable {
    /// There is no socket file at the expected path.
    SocketMissing(std::path::PathBuf),
    /// The socket file exists but connecting to it failed.
    ConnectionFailed {
        socket_path: std::path::PathBuf,
        source: std::io::Error,
    },
}

impl std::fmt::Display for DaemonUnreachable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SocketMissing(socket_path) => write!(
                formatter,
                "daemon socket not found at {}. Start daemon with `enclave daemon start`.",
                socket_path.display()
            ),
            Self::ConnectionFailed {
                socket_path,
                source,
            } => write!(
                formatter,
                "failed to connect to daemon at {}: {source}",
                socket_path.display()
            ),
        }
    }
}

impl std::error::Error for DaemonUnreachable {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::SocketMissing(_) => None,
            Self::ConnectionFailed { source, .. } => Some(source),
        }
    }
}

/// Whether `err` means the daemon could not be reached at all.
pub fn is_daemon_unreachable(err: &anyhow::Error) -> bool {
    err.downcast_ref::<DaemonUnreachable>().is_some()
}

pub fn send_request(socket_path: &Path, action: &str, params: Value) -> Result<Value> {
    let _total = crate::perf::Timer::new("cli.total");
    let _connect = crate::perf::Timer::new("cli.connect");
    let mut stream = connect_daemon(socket_path)?;
    drop(_connect);
    let _write = crate::perf::Timer::new("cli.request_write");
    write_request(&mut stream, action, params)?;
    drop(_write);
    let _read = crate::perf::Timer::new("cli.response_read");
    let line = read_response_line(stream)?;
    drop(_read);
    parse_response_payload(&line)
}

fn connect_daemon(socket_path: &Path) -> Result<UnixStream> {
    if !socket_path.exists() {
        return Err(anyhow::Error::new(DaemonUnreachable::SocketMissing(
            socket_path.to_path_buf(),
        )));
    }
    crate::fsutil::verify_secure_socket(socket_path)?;
    let stream = UnixStream::connect(socket_path).map_err(|err| {
        anyhow::Error::new(DaemonUnreachable::ConnectionFailed {
            socket_path: socket_path.to_path_buf(),
            source: err,
        })
    })?;
    stream
        .set_read_timeout(Some(CLIENT_IO_TIMEOUT))
        .context("failed to set daemon read timeout")?;
    stream
        .set_write_timeout(Some(CLIENT_IO_TIMEOUT))
        .context("failed to set daemon write timeout")?;
    Ok(stream)
}

fn write_request(stream: &mut UnixStream, action: &str, params: Value) -> Result<()> {
    let request = json!({
        "action": action,
        "params": params,
    });
    let payload = serde_json::to_vec(&request)?;
    stream.write_all(&payload)?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    Ok(())
}

fn read_response_line(stream: UnixStream) -> Result<String> {
    let mut line = String::new();
    let mut reader = BufReader::new(stream);
    let mut limited = reader.by_ref().take((MAX_RESPONSE_BYTES + 1) as u64);
    limited
        .read_line(&mut line)
        .context("failed reading daemon response")?;
    if line.trim().is_empty() {
        bail!("daemon returned empty response");
    }
    if line.len() > MAX_RESPONSE_BYTES {
        bail!("daemon response exceeded maximum size");
    }
    if !line.ends_with('\n') {
        bail!("daemon response was not newline-terminated");
    }
    Ok(line)
}

fn parse_response_payload(line: &str) -> Result<Value> {
    let response: ResponsePayload =
        serde_json::from_str(line).context("invalid daemon response payload")?;
    LAST_OPERATION_ID.with(|id| *id.borrow_mut() = response.operation_id.clone());
    if response.ok {
        return Ok(response.result.unwrap_or(Value::Null));
    }
    let message = response
        .error
        .unwrap_or_else(|| "daemon returned an unknown error".to_string());
    match response.operation_id {
        Some(id) => bail!("{message} (operation {id})"),
        None => bail!("{message}"),
    }
}

#[cfg(test)]
#[path = "../tests/src/client.rs"]
mod tests;
