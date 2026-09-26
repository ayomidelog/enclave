//! Tests for the operation journal.
//!
//! A journal record is written before its operation starts and closed when it finishes,
//! so the fixtures here build records directly rather than by running an operation.

use super::*;
use serde_json;
use std::fs;

/// Write one journal record directly, so a retention test can build a journal far
/// larger than the limit without paying a durable write for each record.
fn write_record(root: &std::path::Path, id: &str, status: OperationStatus, updated_at: &str) {
    let record = OperationRecord {
        id: id.to_string(),
        kind: "workspace.start".to_string(),
        target: "sb/ws".to_string(),
        status,
        phase: "complete".to_string(),
        started_at: updated_at.to_string(),
        updated_at: updated_at.to_string(),
        error: None,
    };
    fs::create_dir_all(root).expect("create journal directory");
    fs::write(
        root.join(format!("{id}.json")),
        serde_json::to_vec(&record).expect("encode record"),
    )
    .expect("write record");
}

fn retention_state(label: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "enclave-operation-retention-{}-{label}",
        uuid::Uuid::new_v4()
    ))
}

mod latest;
mod record;
mod retention;
