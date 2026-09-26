//! The workspace data model.
//!
//! A workspace is described by its status, its limits, its record, and the
//! reports its commands return. Each is a separate subject, so each is a
//! separate module and this one is the API over them.

mod limits;
mod metadata;
mod reports;
mod status;

pub use limits::{WorkspaceLimits, WorkspaceLimitsUpdate};
pub use metadata::{NamespaceRefs, WorkspaceMetadata};
pub use reports::{
    WorkspaceCpResult, WorkspaceExecResult, WorkspaceListItem, WorkspaceLogsResult,
    WorkspaceResizeResult, WorkspaceRuntimeInfo, WorkspaceSnapshotArchiveInfo,
    WorkspaceSnapshotInfo, WorkspaceStatsReport, WorkspaceStatusReport,
};
pub use status::WorkspaceStatus;
