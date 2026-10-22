//! Durable records for commands running in workspaces.
//!
//! A job record answers what a workspace is running, not whether the daemon is
//! still alive to supervise it. It carries the command line, the process-group
//! identity, the mode, and the terminal outcome. The live daemon keeps the
//! command lease separately, so an idle sweep never mistakes a running
//! foreground command for an abandoned workspace.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::workspace::activity::{now, parse_timestamp};

const JOBS_DIR: &str = "jobs";

/// Whether a job holds the workspace open for a caller or runs detached.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum JobMode {
    Foreground,
    Background,
}

impl JobMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Foreground => "foreground",
            Self::Background => "background",
        }
    }
}

/// The lifecycle state of one job.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Running,
    Exited,
    Failed,
    Cancelled,
}

impl JobStatus {
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Running)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Exited => "exited",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// One command run through the daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRecord {
    pub id: String,
    pub sandbox_id: String,
    pub workspace_id: String,
    pub mode: JobMode,
    pub command: Vec<String>,
    pub started_at: String,
    #[serde(default)]
    pub finished_at: Option<String>,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub starttime_ticks: Option<u64>,
    #[serde(default)]
    pub process_group: Option<i32>,
    pub status: JobStatus,
    #[serde(default)]
    pub exit_code: Option<i32>,
    /// Optional background lifetime. A zero value means no TTL.
    #[serde(default)]
    pub ttl_secs: Option<u64>,
    /// Whether this background job keeps its workspace from being idle-stopped.
    #[serde(default)]
    pub keep_running: bool,
    #[serde(default)]
    pub error: Option<String>,
}

impl JobRecord {
    pub fn new(sandbox_id: &str, workspace_id: &str, mode: JobMode, command: &[String]) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            sandbox_id: sandbox_id.to_string(),
            workspace_id: workspace_id.to_string(),
            mode,
            command: command.to_vec(),
            started_at: now(),
            finished_at: None,
            pid: None,
            starttime_ticks: None,
            process_group: None,
            status: JobStatus::Running,
            exit_code: None,
            ttl_secs: None,
            keep_running: false,
            error: None,
        }
    }

    pub fn command_line(&self) -> String {
        self.command.join(" ")
    }

    /// Whether a background job has outlived its explicit TTL.
    pub fn expired(&self, at: chrono::DateTime<chrono::Utc>) -> bool {
        let Some(ttl) = self.ttl_secs.filter(|ttl| *ttl > 0) else {
            return false;
        };
        parse_timestamp(&self.started_at)
            .is_some_and(|started| (at - started).num_seconds() >= ttl as i64)
    }
}

/// The directory holding all job records.
pub fn jobs_dir(state_dir: &Path) -> PathBuf {
    state_dir.join(JOBS_DIR)
}

fn job_path(state_dir: &Path, job_id: &str) -> Result<PathBuf> {
    if job_id.is_empty()
        || !job_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        bail!("invalid job id {job_id:?}");
    }
    Ok(jobs_dir(state_dir).join(format!("{job_id}.json")))
}

/// Write a job record atomically.
pub fn persist(state_dir: &Path, job: &JobRecord) -> Result<()> {
    fs::create_dir_all(jobs_dir(state_dir))
        .with_context(|| format!("failed to create {}", jobs_dir(state_dir).display()))?;
    let path = job_path(state_dir, &job.id)?;
    let raw = serde_json::to_vec_pretty(job)?;
    crate::fsutil::write_file_atomic(&path, &raw, 0o600)
        .with_context(|| format!("failed to persist job {}", path.display()))
}

/// Read one job record.
pub fn load(state_dir: &Path, job_id: &str) -> Result<JobRecord> {
    let path = job_path(state_dir, job_id)?;
    let raw = fs::read_to_string(&path)
        .with_context(|| format!("failed to read job {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("invalid job record {}", path.display()))
}

/// Every job record, oldest first.
pub fn list(state_dir: &Path) -> Result<Vec<JobRecord>> {
    let dir = jobs_dir(state_dir);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", dir.display()))
        }
    };
    let mut jobs = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.path().extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let raw = fs::read_to_string(entry.path())?;
        if let Ok(job) = serde_json::from_str::<JobRecord>(&raw) {
            jobs.push(job);
        }
    }
    jobs.sort_by(|a, b| a.started_at.cmp(&b.started_at));
    Ok(jobs)
}

/// Jobs for one workspace, oldest first.
pub fn list_for_workspace(
    state_dir: &Path,
    sandbox_id: &str,
    workspace_id: &str,
) -> Result<Vec<JobRecord>> {
    Ok(list(state_dir)?
        .into_iter()
        .filter(|job| job.sandbox_id == sandbox_id && job.workspace_id == workspace_id)
        .collect())
}

/// Mark a job finished and persist the result.
pub fn finish(
    state_dir: &Path,
    job_id: &str,
    status: JobStatus,
    exit_code: Option<i32>,
    error: Option<String>,
) -> Result<()> {
    let mut job = load(state_dir, job_id)?;
    job.status = status;
    job.exit_code = exit_code;
    job.error = error;
    job.finished_at = Some(now());
    persist(state_dir, &job)
}

/// Finish a job only if it is still running.
///
/// A TTL cancellation and the supervisor thread that observes the resulting
/// signal can race. The terminal state that got there first wins; a later
/// `wait()` result must not turn a deliberate cancellation into a failure.
pub fn finish_if_running(
    state_dir: &Path,
    job_id: &str,
    status: JobStatus,
    exit_code: Option<i32>,
    error: Option<String>,
) -> Result<bool> {
    let mut job = load(state_dir, job_id)?;
    if job.status != JobStatus::Running {
        return Ok(false);
    }
    job.status = status;
    job.exit_code = exit_code;
    job.error = error;
    job.finished_at = Some(now());
    persist(state_dir, &job)?;
    Ok(true)
}

/// Mark every running job owned by a workspace as cancelled.
///
/// A workspace stop kills the runtime cgroup, including detached jobs. Their
/// durable records must say that happened rather than remain `running` until the
/// next daemon startup notices the process groups are gone.
pub fn cancel_running_for_workspace(
    state_dir: &Path,
    sandbox_id: &str,
    workspace_id: &str,
    reason: &str,
) -> Result<usize> {
    let mut changed = 0usize;
    for mut job in list_for_workspace(state_dir, sandbox_id, workspace_id)? {
        if job.status != JobStatus::Running {
            continue;
        }
        job.status = JobStatus::Cancelled;
        job.finished_at = Some(now());
        job.error = Some(reason.to_string());
        persist(state_dir, &job)?;
        changed += 1;
    }
    Ok(changed)
}

/// A short label for a command line that is safe to show in a status table.
pub fn describe_command(command: &[String]) -> String {
    const LIMIT: usize = 160;
    let rendered = command.join(" ");
    if rendered.chars().count() <= LIMIT {
        return rendered;
    }
    let mut shortened = rendered.chars().take(LIMIT).collect::<String>();
    shortened.push_str("...");
    shortened
}
