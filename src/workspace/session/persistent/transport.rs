//! Sending one command to a live helper and reading its response.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::workspace::types::WorkspaceMetadata;

use super::PersistentHelper;

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
}

pub(super) fn send_command(
    helper: &mut PersistentHelper,
    runtime_pid: u32,
    runtime_starttime_ticks: u64,
    workspace: &WorkspaceMetadata,
    cwd: &str,
    command: &[String],
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
                return Err(error)
                    .with_context(|| format!("failed to connect to {}", helper.socket.display()))
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
    serde_json::to_writer(&mut stream, &request).context("failed to encode persistent command")?;
    stream
        .write_all(b"\n")
        .context("failed to terminate persistent command")?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .context("failed to finish persistent command request")?;
    let mut response_line = String::new();
    BufReader::new(stream)
        .read_line(&mut response_line)
        .context("failed to read persistent command response")?;
    if response_line.is_empty() {
        bail!("persistent workspace session helper closed the connection")
    }
    let response: CommandResponse = serde_json::from_str(&response_line)
        .context("failed to decode persistent command response")?;
    Ok(PersistentCommandOutput {
        exit_code: response.exit_code,
        stdout: response.stdout,
        stderr: response.stderr,
    })
}
