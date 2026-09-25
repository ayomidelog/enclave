//! What one lifecycle operation is, and how it reports its own progress.
//!
//! A record is written before the work starts and updated as it goes, so the
//! journal answers two questions: what the daemon is doing right now, and what it
//! did last. The status is the part recovery reads, so a record that is not
//! terminal is never treated as finished.

use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::current;

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

/// Build the record for a new journal, reusing the request's operation id when
/// there is one so the CLI, the logs, and the journal all name one operation.
pub(super) fn record_for(kind: impl Into<String>, target: impl Into<String>) -> OperationRecord {
    let mut record = OperationRecord::new(kind, target);
    if let Some(id) = current() {
        record.id = id;
    }
    record
}

fn timestamp() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}
