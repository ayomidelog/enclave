use std::fs;
use std::path::Path;

use crate::operation::OperationStatus;

use super::DoctorCheck;

pub(crate) fn check_operation_journal(state_dir: &Path) -> DoctorCheck {
    let name = "operation_journal";
    let root = state_dir.join("operations");
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return DoctorCheck::ok(name, "no operation journal exists");
        }
        Err(error) => {
            return DoctorCheck::warn(name, &format!("failed to read {}: {error}", root.display()))
        }
    };
    let mut unfinished = Vec::new();
    let mut malformed = 0usize;
    for entry in entries.flatten() {
        if entry
            .path()
            .extension()
            .and_then(|extension| extension.to_str())
            != Some("json")
        {
            continue;
        }
        match fs::read(entry.path())
            .ok()
            .and_then(|raw| serde_json::from_slice::<crate::operation::OperationRecord>(&raw).ok())
        {
            Some(record)
                if matches!(
                    record.status,
                    OperationStatus::Planned | OperationStatus::Running
                ) =>
            {
                unfinished.push(format!("{} {} ({})", record.id, record.kind, record.target));
            }
            Some(_) => {}
            None => malformed += 1,
        }
    }
    if malformed > 0 || !unfinished.is_empty() {
        let mut details = Vec::new();
        if !unfinished.is_empty() {
            details.push(format!("unfinished: {}", unfinished.join("; ")));
        }
        if malformed > 0 {
            details.push(format!("{malformed} malformed journal file(s)"));
        }
        DoctorCheck::warn(name, &details.join("; "))
    } else {
        DoctorCheck::ok(name, "all operation journal records are terminal")
    }
}
