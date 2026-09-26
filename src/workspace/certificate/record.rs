//! What a cleanup proved, and how a failure is named.
//!
//! The record is the value a stop and a destroy hand back; the checks that
//! fill it in run from the verification side.

use serde::{Deserialize, Serialize};

/// One host resource that a workspace was expected to release but did not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupFailure {
    pub resource: String,
    pub detail: String,
}

/// Verified statement about the host resources a stopped workspace owned.
///
/// A registry mutation finishing is not evidence that the host is clean: the
/// runtime, its cgroup, its mounts, its loop device, its network, its published
/// ports, and its runtime markers are independent resources released by
/// independent operations. The certificate records what was actually verified,
/// so callers can tell "metadata deleted" apart from "host fully clean" and so a
/// stop that leaves something behind fails instead of reporting success.
/// The default certificate verifies nothing, so it is incomplete and fails
/// closed. It exists as the deserialization fallback for a response that predates
/// the field, where "no evidence" must not read as "verified clean".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceCleanupCertificate {
    pub workspace_id: String,
    pub runtime_exited: bool,
    pub cgroup_absent: bool,
    pub mounts_absent: bool,
    pub loop_device_absent: bool,
    pub runtime_files_removed: bool,
    /// Per-resource network outcome, when the caller had network state to tear
    /// down. The daemon owns the port publisher and fills the network and port
    /// fields after its own teardown step.
    #[serde(default)]
    pub network_complete: Option<bool>,
    #[serde(default)]
    pub ports_released: Option<bool>,
    /// Whether the workspace directory itself is gone.
    ///
    /// A stop keeps the directory, so this is left unset there; a destroy has to
    /// prove it is gone.
    #[serde(default)]
    pub files_removed: Option<bool>,
    #[serde(default)]
    pub failures: Vec<CleanupFailure>,
    /// What the workspace owned before its teardown ran.
    ///
    /// The per-resource flags above say which checks passed. This says what the
    /// checks were run against, so an operator can see the inventory a cleanup
    /// started from and the report can prove the diff is empty rather than only
    /// that each named check succeeded.
    #[serde(default)]
    pub inventory: Option<crate::workspace::inventory::ResourceInventory>,
}

impl WorkspaceCleanupCertificate {
    /// Record the inventory the teardown started from, and fail for every resource
    /// that is still on the host.
    ///
    /// This is the check that turns "the cleanup calls returned success" into
    /// "the resources are gone". A resource that survives is named by kind and
    /// identity, so the report is actionable rather than a count.
    pub fn with_inventory(
        mut self,
        inventory: crate::workspace::inventory::ResourceInventory,
    ) -> Self {
        for resource in inventory.surviving() {
            self.failures.push(CleanupFailure {
                resource: resource.kind().as_str().to_string(),
                detail: format!("{} was not released", resource.describe()),
            });
        }
        self.inventory = Some(inventory);
        self
    }

    /// Record the outcome of the caller's own port release step.
    pub fn with_ports_released(mut self, released: bool) -> Self {
        self.ports_released = Some(released);
        if !released {
            self.failures.push(CleanupFailure {
                resource: "ports".to_string(),
                detail: "published port listeners are still active".to_string(),
            });
        }
        self
    }

    /// Every mandatory check passed and no resource reported a failure.
    pub fn is_complete(&self) -> bool {
        self.failures.is_empty()
            && self.runtime_exited
            && self.cgroup_absent
            && self.mounts_absent
            && self.loop_device_absent
            && self.runtime_files_removed
            && self.network_complete.unwrap_or(true)
            && self.ports_released.unwrap_or(true)
            && self.files_removed.unwrap_or(true)
    }

    /// Render the failures as one line suitable for an error message.
    pub fn failure_summary(&self) -> String {
        self.failures
            .iter()
            .map(|failure| format!("{}: {}", failure.resource, failure.detail))
            .collect::<Vec<_>>()
            .join("; ")
    }

    pub(super) fn record(&mut self, resource: &str, passed: bool, detail: impl Into<String>) {
        if !passed {
            self.failures.push(CleanupFailure {
                resource: resource.to_string(),
                detail: detail.into(),
            });
        }
    }
}
