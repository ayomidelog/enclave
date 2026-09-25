//! Where a workspace is in its lifecycle.
//!
//! The status is what every other part of the system reads to decide whether a
//! workspace may be started, stopped, or reported as usable.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum WorkspaceStatus {
    /// A runtime launch is in progress and the workspace is not usable yet.
    Starting,
    Running,
    /// Teardown is in progress. The runtime may still exist until the
    /// transition to `Stopped` commits.
    Stopping,
    #[default]
    Stopped,
}

impl WorkspaceStatus {
    /// A lifecycle operation is in flight. Callers must not start a competing
    /// operation, and the recorded runtime identity may not be final yet.
    pub fn is_transitional(&self) -> bool {
        matches!(self, Self::Starting | Self::Stopping)
    }

    /// The workspace may own a live runtime right now.
    pub fn may_have_runtime(&self) -> bool {
        matches!(self, Self::Starting | Self::Running | Self::Stopping)
    }

    /// The workspace is expected to be usable by commands.
    pub fn is_running(&self) -> bool {
        matches!(self, Self::Running)
    }

    /// Lowercase label used in user-facing output and error messages.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Stopping => "stopping",
            Self::Stopped => "stopped",
        }
    }
}
