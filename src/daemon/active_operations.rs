//! The lifecycle operations a daemon is running right now.
//!
//! Shutdown has to answer two questions: is anything still running, and if so,
//! which operations will need repair when the daemon comes back. The journals
//! answer the second question durably, but nothing answered the first, so a
//! shutdown either waited silently or gave no clue about what it interrupted.

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// One lifecycle operation in flight.
#[derive(Debug, Clone)]
pub(crate) struct ActiveOperation {
    pub(crate) id: String,
    pub(crate) action: String,
    pub(crate) target: String,
    started: Instant,
}

impl ActiveOperation {
    pub(crate) fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// Render the operation for a status or health response.
    pub(crate) fn describe(&self) -> serde_json::Value {
        serde_json::json!({
            "operation_id": self.id,
            "action": self.action,
            "target": self.target,
            "elapsed_secs": self.elapsed().as_secs(),
        })
    }
}

#[derive(Default)]
pub(crate) struct ActiveOperations {
    state: Mutex<BTreeMap<String, ActiveOperation>>,
    idle: Condvar,
}

impl ActiveOperations {
    /// Record an operation as running until the returned guard is dropped.
    pub(crate) fn begin(
        self: &Arc<Self>,
        id: &str,
        action: &str,
        target: &str,
    ) -> ActiveOperationGuard {
        let operation = ActiveOperation {
            id: id.to_string(),
            action: action.to_string(),
            target: target.to_string(),
            started: Instant::now(),
        };
        self.lock().insert(operation.id.clone(), operation);
        ActiveOperationGuard {
            active: Arc::clone(self),
            id: id.to_string(),
        }
    }

    /// The operations running now.
    pub(crate) fn in_flight(&self) -> Vec<ActiveOperation> {
        self.lock().values().cloned().collect()
    }

    /// Wait up to the grace period for every operation to finish, then report
    /// whatever is still running.
    pub(crate) fn wait_for_drain(&self, grace: Duration) -> Vec<ActiveOperation> {
        let deadline = Instant::now() + grace;
        let mut state = self.lock();
        while !state.is_empty() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            let (next, _) = self
                .idle
                .wait_timeout(state, remaining)
                .unwrap_or_else(|error| error.into_inner());
            state = next;
        }
        state.values().cloned().collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, ActiveOperation>> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }
}

/// Removes its operation from the registry when the request finishes.
pub(crate) struct ActiveOperationGuard {
    active: Arc<ActiveOperations>,
    id: String,
}

impl Drop for ActiveOperationGuard {
    fn drop(&mut self) {
        self.active.lock().remove(&self.id);
        self.active.idle.notify_all();
    }
}

#[cfg(test)]
#[path = "../../tests/src/daemon/active_operations.rs"]
mod tests;
