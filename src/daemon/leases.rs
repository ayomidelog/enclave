//! Sandbox-scoped lifecycle leases.
//!
//! The daemon serves requests from a pool of worker threads, so two lifecycle
//! requests can act on the same sandbox at the same time: a stop and a start of
//! one workspace, a sandbox-wide wipe and a workspace start, a resize and a
//! snapshot restore. Those combinations interleave host side effects — mounts,
//! cgroups, veth pairs, the workspace disk image — with the registry commits
//! that describe them.
//!
//! A lease serializes exactly the operations that overlap and lets everything
//! else through. Unrelated sandboxes always proceed. Unrelated workspaces in one
//! sandbox proceed. An operation that touches every workspace of a sandbox
//! excludes the workspace-scoped operations in that sandbox, and an operation
//! that touches every sandbox excludes everything.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use anyhow::{bail, Result};

/// Longest a request waits for an overlapping operation before failing.
///
/// Waiting forever would let one stuck operation consume the whole worker pool.
/// The bound is generous enough to cover a slow stop or a large destroy, and it
/// stays below the client's 300s read timeout so the caller receives the lease
/// diagnostic instead of a generic timeout.
const LEASE_WAIT_TIMEOUT: Duration = Duration::from_secs(240);

/// Which resources a lifecycle operation needs exclusive access to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LeaseScope {
    /// The operation touches every sandbox, so nothing else runs with it.
    Global,
    /// The operation touches every workspace in one sandbox.
    Sandbox(String),
    /// The operation touches one workspace, and shares the sandbox with the
    /// other workspaces in it.
    Workspace { sandbox: String, workspace: String },
}

/// The lifecycle leases held by one daemon.
#[derive(Default)]
pub(crate) struct LifecycleLeases {
    global: Arc<Gate>,
    sandboxes: Mutex<HashMap<String, Arc<Gate>>>,
    workspaces: Mutex<HashMap<(String, String), Arc<Gate>>>,
}

impl LifecycleLeases {
    /// Wait for exclusive access to the resources a scope covers.
    ///
    /// The returned guard releases on drop, including when the operation
    /// panics, so a failed request cannot leave a lease held.
    pub(crate) fn acquire(&self, scope: LeaseScope) -> Result<LeaseGuard> {
        let mut guards = Vec::new();
        match scope {
            LeaseScope::Global => guards.push(self.global.acquire_exclusive()?),
            LeaseScope::Sandbox(sandbox) => {
                // Every non-global operation shares the global gate, so an
                // operation that spans the whole host excludes all of them.
                guards.push(self.global.acquire_shared()?);
                let gate = self.sandbox_gate(&sandbox);
                guards.push(gate.acquire_exclusive()?);
            }
            LeaseScope::Workspace { sandbox, workspace } => {
                guards.push(self.global.acquire_shared()?);
                // The workspace lock is taken first so that two operations on
                // the same workspace serialize before either competes for the
                // sandbox, and the sandbox lock is taken shared so unrelated
                // workspaces in it keep running.
                let workspace_gate = self.workspace_gate(&sandbox, &workspace);
                guards.push(workspace_gate.acquire_exclusive()?);
                let sandbox_gate = self.sandbox_gate(&sandbox);
                guards.push(sandbox_gate.acquire_shared()?);
            }
        }
        Ok(LeaseGuard { _guards: guards })
    }

    fn sandbox_gate(&self, sandbox: &str) -> Arc<Gate> {
        let mut gates = self
            .sandboxes
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        Arc::clone(gates.entry(sandbox.to_string()).or_default())
    }

    fn workspace_gate(&self, sandbox: &str, workspace: &str) -> Arc<Gate> {
        let mut gates = self
            .workspaces
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        Arc::clone(
            gates
                .entry((sandbox.to_string(), workspace.to_string()))
                .or_default(),
        )
    }
}

/// Releases the leases it holds when it is dropped.
pub(crate) struct LeaseGuard {
    _guards: Vec<GateGuard>,
}

/// A gate that admits any number of shared holders or one exclusive holder.
///
/// Admission is starvation free: while an exclusive holder is waiting, new
/// shared holders are held back, so a sandbox-wide operation cannot be delayed
/// forever by a stream of workspace-scoped ones.
#[derive(Default)]
struct Gate {
    state: Mutex<GateState>,
    ready: Condvar,
}

#[derive(Default)]
struct GateState {
    shared: usize,
    exclusive: bool,
    waiting_exclusive: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum GateMode {
    Shared,
    Exclusive,
}

struct GateGuard {
    gate: Arc<Gate>,
    mode: GateMode,
}

impl Gate {
    fn acquire_shared(self: &Arc<Self>) -> Result<GateGuard> {
        self.acquire(GateMode::Shared)
    }

    fn acquire_exclusive(self: &Arc<Self>) -> Result<GateGuard> {
        self.acquire(GateMode::Exclusive)
    }

    fn acquire(self: &Arc<Self>, mode: GateMode) -> Result<GateGuard> {
        let deadline = Instant::now() + LEASE_WAIT_TIMEOUT;
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if mode == GateMode::Exclusive {
            state.waiting_exclusive += 1;
        }
        let result = loop {
            let admitted = match mode {
                GateMode::Shared => !state.exclusive && state.waiting_exclusive == 0,
                GateMode::Exclusive => !state.exclusive && state.shared == 0,
            };
            if admitted {
                break Ok(());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break Err(());
            }
            let (next, _) = self
                .ready
                .wait_timeout(state, remaining)
                .unwrap_or_else(|error| error.into_inner());
            state = next;
        };
        if mode == GateMode::Exclusive {
            state.waiting_exclusive -= 1;
        }
        match result {
            Ok(()) => {
                match mode {
                    GateMode::Shared => state.shared += 1,
                    GateMode::Exclusive => state.exclusive = true,
                }
                drop(state);
                Ok(GateGuard {
                    gate: Arc::clone(self),
                    mode,
                })
            }
            Err(()) => {
                let detail = describe(&state);
                drop(state);
                bail!(
                    "timed out after {}s waiting for another lifecycle operation to finish ({detail})",
                    LEASE_WAIT_TIMEOUT.as_secs()
                )
            }
        }
    }
}

impl Drop for GateGuard {
    fn drop(&mut self) {
        let mut state = self
            .gate
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        match self.mode {
            GateMode::Shared => state.shared = state.shared.saturating_sub(1),
            GateMode::Exclusive => state.exclusive = false,
        }
        drop(state);
        self.gate.ready.notify_all();
    }
}

/// Human readable description of who is holding a gate, for the timeout error.
fn describe(state: &MutexGuard<'_, GateState>) -> String {
    let mut held = Vec::new();
    if state.exclusive {
        held.push("one exclusive operation".to_string());
    }
    if state.shared > 0 {
        held.push(format!("{} workspace operation(s)", state.shared));
    }
    if state.waiting_exclusive > 0 {
        held.push(format!("{} waiting operation(s)", state.waiting_exclusive));
    }
    if held.is_empty() {
        return "no holder recorded".to_string();
    }
    held.join(", ")
}

#[cfg(test)]
#[path = "../../tests/src/daemon/leases.rs"]
mod tests;
