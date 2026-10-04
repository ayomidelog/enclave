use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::workspace::session::MAX_HELPER_OUTPUT_BYTES;

/// How many bytes of JSON one captured byte can become.
///
/// `serde_json` writes a control character below `0x20` as `\u00XX` — six bytes
/// for one — and passes every other byte through, so six is the worst case and
/// the one the response ceiling is sized against.
const JSON_ESCAPE_WORST_CASE: usize = 6;

/// The largest daemon response the client reads.
///
/// A `workspace exec` result carries the command's captured stdout and stderr,
/// each capped at [`MAX_HELPER_OUTPUT_BYTES`], and both are JSON-escaped onto the
/// single line the daemon writes, so the largest reply is twelve bytes of JSON
/// per captured byte — the two streams times the worst-case escape of six —
/// plus the envelope. The ceiling is derived from that capture cap rather than
/// chosen independently, so the two cannot drift: a ceiling below the daemon's
/// maximum rejects a reply the daemon was entitled to send, and it rejects it
/// after the command has already run. It was 512 KiB, thirty-two times below
/// what the daemon may send, so a `workspace exec` that printed more failed with
/// "daemon response exceeded maximum size".
const MAX_RESPONSE_BYTES: usize = 2 * MAX_HELPER_OUTPUT_BYTES * JSON_ESCAPE_WORST_CASE + 64 * 1024;
const CLIENT_IO_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, Deserialize)]
struct ResponsePayload {
    ok: bool,
    result: Option<Value>,
    error: Option<String>,
    #[serde(default)]
    code: Option<crate::error::ErrorCode>,
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
    let rendered = match response.operation_id {
        Some(id) => format!("{message} (operation {id})"),
        None => message,
    };
    // Carry the daemon's category into the caller's error, so a caller can act
    // on the kind of failure without reading the message it is written in.
    match response.code {
        Some(code) => Err(crate::error::coded(code, rendered)),
        None => bail!("{rendered}"),
    }
}

#[cfg(test)]
#[path = "../tests/src/client.rs"]
mod tests;
