use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

use crate::registry::{with_registry, with_registry_mut};
use crate::sandbox::resolve_sandbox_id;

use super::control::{resolve_workspace_id, set_workspace_stopped};
use super::session;
use super::types::{WorkspaceMetadata, WorkspaceSnapshotArchiveInfo, WorkspaceSnapshotInfo};

pub const DEFAULT_SNAPSHOT_KEEP: usize = 5;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SnapshotMetadata {
    name: String,
    created_at: String,
}

struct SnapshotRestorePaths<'a> {
    snapshot_fs: &'a Path,
    snapshot_home_upper: &'a Path,
    backup_root: &'a Path,
    fs_path: &'a Path,
    upper_path: &'a Path,
    work_path: &'a Path,
    merged_path: &'a Path,
}

mod archive;
mod create;
mod list;
mod paths;
mod restore;

pub use archive::{export_workspace_snapshot_archive, import_workspace_snapshot_archive};
pub use create::create_workspace_snapshot;
pub use list::{gc_workspace_snapshots, list_workspace_snapshots};
pub use restore::restore_workspace_snapshot;

// Shared between the snapshot modules above.
pub(crate) use paths::{
    copy_dir_recursive, default_snapshot_name, ensure_snapshot_layout, read_snapshot_metadata,
    reset_path, snapshot_directory, snapshot_path, snapshots_root, temporary_workspace,
    validate_snapshot_name,
};

#[cfg(test)]
#[path = "../../tests/src/workspace/snapshot.rs"]
mod tests;
