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
            });
        }

        let (raw, truncated) = read_tail_bytes(&log_path, MAX_LOG_READ_BYTES)?;
        let mut content = match tail {
            Some(limit) => tail_lines(&raw, limit),
            None => raw,
        };
        if truncated {
            content = format!(
                "[enclave] log output truncated to last {} bytes\n{}",
                MAX_LOG_READ_BYTES, content
            );
        }
        Ok(WorkspaceLogsResult { content })
    })
}

fn tail_lines(input: &str, limit: usize) -> String {
    let lines: Vec<&str> = input.lines().collect();
    let start = lines.len().saturating_sub(limit);
    lines[start..].join("\n")
}

fn read_tail_bytes(path: &Path, max_bytes: u64) -> Result<(String, bool)> {
    let mut file =
        File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let len = file
        .metadata()
        .with_context(|| format!("failed to stat {}", path.display()))?
        .len();
    let offset = len.saturating_sub(max_bytes);
    if offset > 0 {
        file.seek(SeekFrom::Start(offset))
            .with_context(|| format!("failed to seek {}", path.display()))?;
    }

    let mut raw = Vec::new();
    file.read_to_end(&mut raw)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let content = String::from_utf8_lossy(&raw).to_string();
    Ok((content, offset > 0))
}

#[cfg(test)]
#[path = "../../tests/src/workspace/logs.rs"]
mod tests;
