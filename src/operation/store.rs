//! The journal directory: reading it, closing what a dead daemon left open, and
//! keeping it bounded.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::fsutil::Durability;

use super::journal::persist_record;
use super::record::{OperationRecord, OperationStatus};

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

/// How many terminal journal records are kept.
///
/// The journal is the audit trail of what the daemon did and why, so it is worth
/// keeping. It is also one file per lifecycle operation, and a host that starts and
/// stops workspaces for months accumulates them without bound. Terminal records are
/// therefore trimmed to the newest [`JOURNAL_TERMINAL_LIMIT`]. A record that is not
/// terminal is never removed, because recovery reads those.
///
/// The trim runs at daemon startup rather than on the write path, so no lifecycle
/// operation pays for it, and it returns without reading anything when the journal
/// is already inside the limit, which is the usual case.
pub(crate) const JOURNAL_TERMINAL_LIMIT: usize = 1000;

/// Trim terminal records beyond the retention limit, keeping the newest.
///
/// Returns how many were removed, so the caller can say what it did rather than
/// deleting part of the audit trail silently. A record that cannot be read is left
/// in place: doctor reports it, and removing a file this function does not
/// understand is not its job.
pub fn prune_terminal_records(state_dir: &Path) -> Result<usize> {
    let root = state_dir.join("operations");
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read operation journal {}", root.display()))
        }
    };

    let mut terminal: Vec<(String, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let Ok(raw) = fs::read(&path) else { continue };
        let Ok(record) = serde_json::from_slice::<OperationRecord>(&raw) else {
            continue;
        };
        if matches!(
            record.status,
            OperationStatus::Planned | OperationStatus::Running
        ) {
            continue;
        }
        terminal.push((record.updated_at, path));
    }

    if terminal.len() <= JOURNAL_TERMINAL_LIMIT {
        return Ok(0);
    }
    // Newest first, so everything past the limit is the oldest.
    terminal.sort_by(|left, right| right.0.cmp(&left.0));
    let mut removed = 0;
    for (_, path) in terminal.into_iter().skip(JOURNAL_TERMINAL_LIMIT) {
        match fs::remove_file(&path) {
            Ok(()) => removed += 1,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to remove journal record {}", path.display()))
            }
        }
    }
    Ok(removed)
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
