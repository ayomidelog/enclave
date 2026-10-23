//! Sending one command to a live helper and reading its response.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::workspace::types::WorkspaceMetadata;

use super::PersistentHelper;

/// Marks the one failure that means the helper itself stopped answering.
///
/// The caller kills and drops a helper that raises this, so the next command for
/// the workspace starts a fresh one instead of queueing behind a dead socket.
pub(crate) const HELPER_TIMEOUT_PREFIX: &str = "persistent_helper_timeout";

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
    // The helper answers only after the command's own child exits, so this bound is
    // also the longest one command may run. It is generous by default and only
    // exists so a command that never exits cannot pin this thread, and with it an
    // exec worker and the workspace's command mutex, forever.
    let deadline = crate::deadlines::helper_response();
    stream
        .set_read_timeout(Some(deadline.value))
        .context("failed to bound the persistent command response wait")?;
    let mut response_line = String::new();
    BufReader::new(stream)
        .read_line(&mut response_line)
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => anyhow::anyhow!(
                "{HELPER_TIMEOUT_PREFIX}: the workspace helper did not answer within {:?} (raise {} to allow longer commands)",
                deadline.value,
                deadline.variable
            ),
            _ => anyhow::Error::new(error).context("failed to read persistent command response"),
        })?;
    if response_line.is_empty() {
        bail!("persistent workspace session helper closed the connection")
    }
    let response: CommandResponse = serde_json::from_str(&response_line)
        .context("failed to decode persistent command response")?;
    Ok(PersistentCommandOutput {
        exit_code: response.exit_code,
        stdout: response.stdout,
        stderr: response.stderr,
        helper_pid: None,
    })
}
