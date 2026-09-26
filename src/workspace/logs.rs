use std::fs;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::Write;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use chrono::{SecondsFormat, Utc};

use crate::registry::with_registry;
use crate::sandbox::resolve_sandbox_id;

use super::control::resolve_workspace_id;
use super::session;
use super::types::{WorkspaceLogsResult, WorkspaceMetadata};

const MAX_LOG_READ_BYTES: u64 = 1_048_576;

/// Largest slice of appended log a single follow poll returns.
///
/// A follower polls repeatedly, so it does not need one response to carry
/// everything: it needs the next chunk. This is well below the client's response
/// cap even after JSON escaping, which doubles the size of a log full of
/// newlines, so a fast writer cannot make the follower fail with an oversized
/// response instead of printing the log.
const MAX_LOG_DELTA_BYTES: u64 = 128 * 1024;

pub fn append_workspace_command_log(
    workspace: &WorkspaceMetadata,
    cwd: &str,
    command: &[String],
    exit_code: i32,
    stdout: &str,
    stderr: &str,
) -> Result<()> {
    let log_path = session::runtime_log_file(workspace);
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let timestamp = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let sanitized_cwd = sanitize_log_header_field(cwd);
    let sanitized_command = sanitize_log_header_field(&command.join(" "));
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("failed to open {}", log_path.display()))?;

    writeln!(
        log,
        "[{}] workspace command\ncwd: {}\ncommand: {}\nexit_code: {}\nstdout:\n{}\nstderr:\n{}\n---",
        timestamp,
        sanitized_cwd,
        sanitized_command,
        exit_code,
        stdout,
        stderr
    )
    .with_context(|| format!("failed to write {}", log_path.display()))?;

    Ok(())
}

fn sanitize_log_header_field(input: &str) -> String {
    input.chars().flat_map(char::escape_default).collect()
}

pub fn workspace_logs(
    state_dir: &Path,
    sandbox_selector: &str,
    workspace_selector: &str,
    tail: Option<usize>,
    offset: Option<u64>,
    stream_id: Option<&str>,
) -> Result<WorkspaceLogsResult> {
    with_registry(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, sandbox_selector)?;
        let sandbox = registry
            .sandboxes
            .get(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_id))?;
        let workspace_id = resolve_workspace_id(sandbox, workspace_selector)?;
        let workspace = sandbox
            .workspaces
            .get(&workspace_id)
            .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_id))?;

        let log_path = session::runtime_log_file(workspace);
        if !log_path.exists() {
            return Ok(WorkspaceLogsResult {
                content: String::new(),
                next_offset: 0,
                reset: offset.is_some(),
                stream_id: None,
                has_more: false,
            });
        }
        if let Some(offset) = offset {
            return read_log_delta(&log_path, offset, stream_id);
        }

        let tail_read = read_tail_bytes(&log_path, MAX_LOG_READ_BYTES)?;
        let mut content = match tail {
            Some(limit) => tail_lines(&tail_read.content, limit),
            None => tail_read.content,
        };
        if tail_read.truncated {
            content = format!(
                "[enclave] log output truncated to last {} bytes\n{}",
                MAX_LOG_READ_BYTES, content
            );
        }
        Ok(WorkspaceLogsResult {
            content,
            next_offset: tail_read.end_offset,
            reset: false,
            stream_id: Some(tail_read.stream_id),
            has_more: false,
        })
    })
}

/// Read the bytes appended since `offset`.
///
/// An offset only means something for the file it was taken from, so a
/// truncated file (its length fell below the offset) and a replaced file (a
/// different inode at the same path) both answer with a reset rather than with
/// whatever happens to live at that offset now.
fn read_log_delta(
    path: &Path,
    offset: u64,
    expected_stream_id: Option<&str>,
) -> Result<WorkspaceLogsResult> {
    let mut file =
        File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let metadata = file.metadata()?;
    let stream_id = log_stream_id(&metadata);
    let replaced = expected_stream_id.is_some_and(|expected| expected != stream_id);
    if replaced || metadata.len() < offset {
        let tail_read = read_tail_bytes(path, MAX_LOG_READ_BYTES)?;
        return Ok(WorkspaceLogsResult {
            content: tail_read.content,
            next_offset: tail_read.end_offset,
            reset: true,
            stream_id: Some(tail_read.stream_id),
            has_more: false,
        });
    }
    file.seek(SeekFrom::Start(offset))?;
    // Take a bounded slice rather than everything: the follower comes back for
    // the rest, and an unbounded slice would exceed the response cap once the
    // JSON escaping is counted.
    let available = metadata.len().saturating_sub(offset);
    let limit = available.min(MAX_LOG_DELTA_BYTES);
    let mut raw = Vec::with_capacity(limit as usize);
    std::io::Read::take(&mut file, limit).read_to_end(&mut raw)?;
    // Read the offset back from the handle rather than computing it from the
    // length taken before the read, so a write racing the read cannot make the
    // next poll repeat bytes.
    let next_offset = file.stream_position()?;
    Ok(WorkspaceLogsResult {
        content: String::from_utf8_lossy(&raw).to_string(),
        next_offset,
        reset: false,
        stream_id: Some(stream_id),
        // The slice was bounded, so anything past it is waiting.
        has_more: limit < available,
    })
}

fn tail_lines(input: &str, limit: usize) -> String {
    let lines: Vec<&str> = input.lines().collect();
    let start = lines.len().saturating_sub(limit);
    lines[start..].join("\n")
}

struct TailRead {
    content: String,
    truncated: bool,
    end_offset: u64,
    stream_id: String,
}

fn read_tail_bytes(path: &Path, max_bytes: u64) -> Result<TailRead> {
    let mut file =
        File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("failed to stat {}", path.display()))?;
    let offset = metadata.len().saturating_sub(max_bytes);
    if offset > 0 {
        file.seek(SeekFrom::Start(offset))
            .with_context(|| format!("failed to seek {}", path.display()))?;
    }

    let mut raw = Vec::new();
    file.read_to_end(&mut raw)
        .with_context(|| format!("failed to read {}", path.display()))?;
    Ok(TailRead {
        content: String::from_utf8_lossy(&raw).to_string(),
        truncated: offset > 0,
        end_offset: file
            .stream_position()
            .with_context(|| format!("failed to read {}", path.display()))?,
        stream_id: log_stream_id(&metadata),
    })
}

/// A stable identity for a log file: the device and inode it lives on.
///
/// Two different files at the same path differ here even when the second one is
/// longer than the first, which is what makes a replaced log detectable.
fn log_stream_id(metadata: &fs::Metadata) -> String {
    use std::os::linux::fs::MetadataExt;
    format!("{:x}:{:x}", metadata.st_dev(), metadata.st_ino())
}

#[cfg(test)]
#[path = "../../tests/src/workspace/logs.rs"]
mod tests;
