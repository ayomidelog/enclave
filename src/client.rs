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
}

pub fn send_request(socket_path: &Path, action: &str, params: Value) -> Result<Value> {
    let mut stream = connect_daemon(socket_path)?;
    write_request(&mut stream, action, params)?;
    let line = read_response_line(stream)?;
    parse_response_payload(&line)
}

fn connect_daemon(socket_path: &Path) -> Result<UnixStream> {
    if !socket_path.exists() {
        bail!(
            "daemon socket not found at {}. Start daemon with `enclave daemon start`.",
            socket_path.display()
        );
    }
    crate::fsutil::verify_secure_socket(socket_path)?;
    let stream = UnixStream::connect(socket_path)
        .with_context(|| format!("failed to connect to daemon at {}", socket_path.display()))?;
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
    if response.ok {
        return Ok(response.result.unwrap_or(Value::Null));
    }
    bail!(
        "{}",
        response
            .error
            .unwrap_or_else(|| "daemon returned an unknown error".to_string())
    )
}
