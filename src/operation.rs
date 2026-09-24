use std::fs;
use std::path::{Path, PathBuf};

use std::cell::RefCell;

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

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
        let mut journal = Self {
            root,
            record: record_for(kind, target),
        };
        journal.persist()?;
        journal.record.begin("starting");
        journal.persist()?;
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
        self.persist()
    }

    pub fn succeed(mut self) -> Result<OperationRecord> {
        self.record.succeed();
        self.persist()?;
        Ok(self.record)
    }

    pub fn fail(mut self, error: impl Into<String>) -> Result<OperationRecord> {
        self.record.fail(error);
        self.persist()?;
        Ok(self.record)
    }

    fn persist(&self) -> Result<()> {
        let path = self.root.join(format!("{}.json", self.record.id));
        let data = serde_json::to_vec_pretty(&self.record)?;
        crate::fsutil::write_file_atomic(&path, &data, 0o600)
            .with_context(|| format!("failed to persist operation journal {}", path.display()))
    }
}

pub fn load(state_dir: &Path, id: &str) -> Result<OperationRecord> {
    let path = state_dir.join("operations").join(format!("{id}.json"));
    let data = fs::read(&path)
        .with_context(|| format!("failed to read operation journal {}", path.display()))?;
    serde_json::from_slice(&data)
        .with_context(|| format!("failed to parse operation journal {}", path.display()))
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
