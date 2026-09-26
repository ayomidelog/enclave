//! Workspace teardown: releasing the host resources a workspace owns.
//!
//! Two callers exist. A stop releases the runtime, cgroups, network state,
//! storage mounts, and `/tmp` while keeping the workspace's files and registry
//! record. A destroy does the same and then removes the files and the record.
//!
//! Both accept a [`CleanupMode`]. Normal mode refuses to report success while
//! anything is still held, so a command that returns clean has actually left the
//! host clean. Force mode gives that guarantee up deliberately: it releases what
//! it can, never touches a resource whose ownership it cannot prove, and reports
//! exactly what it retained so `enclave doctor --repair` can finish the job.

mod artifacts;
mod batch;
mod cgroups;

use serde::{Deserialize, Serialize};

use crate::sandbox::SandboxMetadata;

use super::types::WorkspaceMetadata;

pub(super) use artifacts::{
    cleanup_workspace_artifacts, remove_workspace_directory_after_record_removal,
};
pub(super) use batch::{
    format_network_cleanup_error, run_workspace_stop_cleanup, run_workspace_stop_cleanups,
};
pub(super) use cgroups::remove_workspace_cgroups;

/// A workspace and the sandbox it belongs to, as needed for teardown.
pub(super) struct WorkspaceStopCleanup {
    pub(super) sandbox: SandboxMetadata,
    pub(super) workspace: WorkspaceMetadata,
}

/// How much cleanup a destructive command requires before it reports success.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CleanupMode {
    /// Every mandatory resource must be released. Anything still held fails the
    /// command and the registry record is retained.
    #[default]
    Normal,
    /// Release what can be released and retain the rest. The registry record is
    /// removed either way, so the retained host state is the operator's to
    /// finish with `enclave doctor --repair`.
    Force,
}

impl CleanupMode {
    pub fn is_force(self) -> bool {
        matches!(self, Self::Force)
    }

    /// Lowercase label used in command output.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Force => "force",
        }
    }
}

/// A host resource that a teardown could not release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetainedResource {
    pub resource: String,
    pub detail: String,
}

/// What a teardown released and what it had to leave behind.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupOutcome {
    /// The workspace files were removed.
    pub files_removed: bool,
    /// Resources that are still held, reported in force mode.
    pub retained: Vec<RetainedResource>,
}

impl CleanupOutcome {
    /// Every mandatory resource was released.
    pub fn is_complete(&self) -> bool {
        self.files_removed && self.retained.is_empty()
    }

    /// One line naming everything that was left behind.
    pub fn retained_summary(&self) -> String {
        self.retained
            .iter()
            .map(|item| format!("{}: {}", item.resource, item.detail))
            .collect::<Vec<_>>()
            .join("; ")
    }
}
