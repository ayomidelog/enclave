//! The lifecycle state a request moved its target from and to.
//!
//! A lifecycle response that reports only the state it ended in cannot answer the
//! question an operator actually has: did this change anything? A start of an
//! already-running workspace and a start that launched a runtime both end at
//! `running`, and the difference is visible only in where they started. The pair
//! is recorded for the same reason the operation id is: it is the part of the
//! answer that is not derivable from the result.
//!
//! The previous state is read before the work starts and carried to the end.
//! Reading it afterwards would report the same state twice and claim that every
//! operation changed something.
//!
//! Only requests that act on one sandbox or one workspace carry a transition. A
//! wipe or a repair acts on everything at once, so there is no single target to
//! describe; those report per-target outcomes instead.

use std::path::Path;

use serde_json::Value;

/// The state of a target that is not in the registry.
///
/// A create starts here, and so does a destroy: the record that described the
/// target is gone by the time the response is written.
pub(super) const ABSENT: &str = "absent";

/// The label used when the target's state could not be read.
///
/// A lifecycle operation reads and writes the same registry this does, so by the
/// time a transition is reported the registry was readable. This exists for the
/// read that happens before the work, which can run against a state directory the
/// operation is about to reject.
pub(super) const UNKNOWN: &str = "unknown";

/// Add the state change to an object result.
///
/// A result that is not an object is returned unchanged. Every lifecycle
/// operation answers with an object, and wrapping one that does not would change
/// the shape a client parses for the sake of a field that does not apply.
pub(super) fn with_transition(
    mut result: Value,
    previous: impl Into<String>,
    current: impl Into<String>,
) -> Value {
    if let Some(object) = result.as_object_mut() {
        object.insert("previous_state".to_string(), previous.into().into());
        object.insert("current_state".to_string(), current.into().into());
    }
    result
}

/// The workspace's state before an operation.
pub(super) fn workspace_state_before(state_dir: &Path, sandbox: &str, workspace: &str) -> String {
    crate::workspace::workspace_metadata(state_dir, sandbox, workspace)
        .map(|metadata| metadata.status.as_str().to_string())
        .unwrap_or_else(|_| UNKNOWN.to_string())
}

/// The sandbox's state before an operation.
pub(super) fn sandbox_state_before(state_dir: &Path, sandbox: &str) -> String {
    crate::sandbox::sandbox_status(state_dir, sandbox)
        .map(|report| report.status.as_str().to_string())
        .unwrap_or_else(|_| UNKNOWN.to_string())
}

#[cfg(test)]
#[path = "../../../tests/src/daemon/transition.rs"]
mod tests;
