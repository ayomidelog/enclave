use std::fs;
use std::path::{Path, PathBuf};

use std::cell::RefCell;

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::fsutil::Durability;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OperationStatus {
    Planned,
    Running,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationRecord {
    pub id: String,
    pub kind: String,
    pub target: String,
    pub status: OperationStatus,
    pub phase: String,
    pub started_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub error: Option<String>,
}

impl OperationRecord {
    pub fn new(kind: impl Into<String>, target: impl Into<String>) -> Self {
        let now = timestamp();
        Self {
            id: Uuid::new_v4().to_string(),
            kind: kind.into(),
            target: target.into(),
            status: OperationStatus::Planned,
            phase: "planned".to_string(),
            started_at: now.clone(),
            updated_at: now,
            error: None,
        }
    }

    pub fn begin(&mut self, phase: impl Into<String>) {
        self.status = OperationStatus::Running;
        self.phase = phase.into();
        self.updated_at = timestamp();
        self.error = None;
    }

    pub fn phase(&mut self, phase: impl Into<String>) {
        self.phase = phase.into();
        self.updated_at = timestamp();
    }

    pub fn succeed(&mut self) {
        self.status = OperationStatus::Succeeded;
        self.phase = "complete".to_string();
        self.updated_at = timestamp();
        self.error = None;
    }

    pub fn fail(&mut self, error: impl Into<String>) {
        self.status = OperationStatus::Failed;
        self.phase = "failed".to_string();
        self.updated_at = timestamp();
        self.error = Some(error.into());
    }
}

pub struct Journal {
    root: PathBuf,
    record: OperationRecord,
}

thread_local! {
    /// The operation id of the request this thread is currently serving.
    ///
    /// A daemon request is handled start to finish on one worker thread, so a
    /// thread-local is enough to let the lifecycle code name the same operation
    /// the caller was told about, without threading an id through every
    /// signature. A CLI thread never sets it, so a directly invoked lifecycle
    /// function still gets a fresh id.
    static CURRENT_OPERATION: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Make `id` the operation id for work started on this thread.
pub fn set_current(id: Option<String>) {
    CURRENT_OPERATION.with(|current| *current.borrow_mut() = id);
}

/// The operation id for work started on this thread, if any.
pub fn current() -> Option<String> {
    CURRENT_OPERATION.with(|current| current.borrow().clone())
}

/// A fresh operation id.
pub fn new_id() -> String {
    Uuid::new_v4().to_string()
}

/// Whether `id` is shaped like an operation id.
///
/// A caller may supply an id so a retry can be correlated with the attempt it
/// repeats. The value is used as a file name, so anything that is not a plain
/// UUID is refused and a fresh id is generated instead.
pub fn is_valid_id(id: &str) -> bool {
    Uuid::parse_str(id).is_ok()
}

impl Journal {
    pub fn begin(
        state_dir: &Path,
        kind: impl Into<String>,
        target: impl Into<String>,
    ) -> Result<Self> {
        let root = state_dir.join("operations");
        fs::create_dir_all(&root)
            .with_context(|| format!("failed to create operation journal {}", root.display()))?;
        // One durable write, not two: the record and its starting phase are the
        // same statement, and writing them separately cost an extra fsync of the
        // file and the directory on every operation.
        let mut record = record_for(kind, target);
        record.begin("starting");
        let journal = Self { root, record };
        journal.persist(Durability::Required)?;
        Ok(journal)
    }

    pub fn id(&self) -> &str {
        &self.record.id
    }

    pub fn record(&self) -> &OperationRecord {
        &self.record
    }

    pub fn phase(&mut self, phase: impl Into<String>) -> Result<()> {
        self.record.phase(phase);
        // A phase is a progress note, not a recovery input: what recovery needs
        // is the record's existence and its terminal status, and the registry
        // holds the transitional state that drives rollback. Writing it without
        // fsync keeps the newest note best effort, so a power loss can only lose
        // the detail of how far the operation had got, never a durable claim.
        self.persist(Durability::BestEffort)
    }

    pub fn succeed(mut self) -> Result<OperationRecord> {
        self.record.succeed();
        self.persist(Durability::Required)?;
        Ok(self.record)
    }

    pub fn fail(mut self, error: impl Into<String>) -> Result<OperationRecord> {
        self.record.fail(error);
        self.persist(Durability::Required)?;
        Ok(self.record)
    }

    fn persist(&self, durability: Durability) -> Result<()> {
        persist_record(&self.root, &self.record, durability)
    }
}

fn persist_record(root: &Path, record: &OperationRecord, durability: Durability) -> Result<()> {
    let path = root.join(format!("{}.json", record.id));
    let data = serde_json::to_vec_pretty(record)?;
    crate::fsutil::write_file_atomic_with(&path, &data, 0o600, durability)
        .with_context(|| format!("failed to persist operation journal {}", path.display()))
}

/// Close every record that is still open, recording `reason` as its outcome.
///
/// An operation whose daemon died never writes its own outcome, because the
/// process that would have written it is gone. The record then stays `planned`
/// or `running` forever, so the journal becomes a growing list of operations
/// that look like they are still in flight, and doctor reports each one on every
/// run.
///
/// The daemon calls this once at startup, before it serves a request. At that
/// moment nothing can be in flight, so every open record belongs to a previous
/// daemon and its outcome is knowable: it was interrupted. The caller reconciles
/// the targets' actual state first, so the reason it passes describes what
/// recovery did rather than only that something stopped.
pub fn close_unfinished_records(state_dir: &Path, reason: &str) -> Result<Vec<String>> {
    let root = state_dir.join("operations");
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read operation journal {}", root.display()))
        }
    };

    let mut closed = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        // A record that cannot be read or parsed is a separate finding that
        // doctor reports; it is not a reason to stop closing the others.
        let Ok(raw) = fs::read(&path) else { continue };
        let Ok(mut record) = serde_json::from_slice::<OperationRecord>(&raw) else {
            continue;
        };
        if !matches!(
            record.status,
            OperationStatus::Planned | OperationStatus::Running
        ) {
            continue;
        }
        record.fail(reason);
        persist_record(&root, &record, Durability::Required)?;
        closed.push(record.id);
    }
    Ok(closed)
}

pub fn load(state_dir: &Path, id: &str) -> Result<OperationRecord> {
    let path = state_dir.join("operations").join(format!("{id}.json"));
    let data = fs::read(&path)
        .with_context(|| format!("failed to read operation journal {}", path.display()))?;
    serde_json::from_slice(&data)
        .with_context(|| format!("failed to parse operation journal {}", path.display()))
}

/// The most recently updated operation record, when any exists.
///
/// This is what an operator wants first when a lifecycle command behaved
/// unexpectedly: the last thing the daemon did, and whether it finished. The
/// record is chosen by its update time rather than by file name, because the file
/// name is a UUID and carries no ordering.
pub fn latest(state_dir: &Path) -> Result<Option<OperationRecord>> {
    let root = state_dir.join("operations");
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read operation journal {}", root.display()))
        }
    };

    let mut latest: Option<OperationRecord> = None;
    for entry in entries.flatten() {
        if entry
            .path()
            .extension()
            .and_then(|extension| extension.to_str())
            != Some("json")
        {
            continue;
        }
        // A malformed record is a separate finding that doctor reports; it is not
        // a reason to fail the whole lookup.
        let Ok(raw) = fs::read(entry.path()) else {
            continue;
        };
        let Ok(record) = serde_json::from_slice::<OperationRecord>(&raw) else {
            continue;
        };
        let newer = latest
            .as_ref()
            .is_none_or(|current| record.updated_at > current.updated_at);
        if newer {
            latest = Some(record);
        }
    }
    Ok(latest)
}

/// Build the record for a new journal, reusing the request's operation id when
/// there is one so the CLI, the logs, and the journal all name one operation.
fn record_for(kind: impl Into<String>, target: impl Into<String>) -> OperationRecord {
    let mut record = OperationRecord::new(kind, target);
    if let Some(id) = current() {
        record.id = id;
    }
    record
}

fn timestamp() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
#[path = "../tests/src/operation.rs"]
mod tests;
