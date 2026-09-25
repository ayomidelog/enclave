//! Writing one operation's record as its phases progress.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::fsutil::Durability;

use super::record::{record_for, OperationRecord};

pub struct Journal {
    root: PathBuf,
    record: OperationRecord,
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

pub(super) fn persist_record(
    root: &Path,
    record: &OperationRecord,
    durability: Durability,
) -> Result<()> {
    let path = root.join(format!("{}.json", record.id));
    let data = serde_json::to_vec_pretty(record)?;
    crate::fsutil::write_file_atomic_with(&path, &data, 0o600, durability)
        .with_context(|| format!("failed to persist operation journal {}", path.display()))
}
