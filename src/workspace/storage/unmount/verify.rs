//! Which mounts Enclave owns, and the proof that they are gone.
//!
//! A cleanup function returning Ok means the calls it made did not report a
//! failure, which is not evidence that the host is clean. These helpers answer
//! from the mount table instead, and they separate Enclave's own mounts from
//! mounts the operator placed, because only the first kind is Enclave's failure.

use std::path::Path;

use anyhow::{bail, Result};

use super::detach::mount_holders;
use crate::workspace::types::WorkspaceMetadata;

/// Whether Enclave may unmount this mount.
///
/// Without a provable state directory Enclave cannot tell its own mounts from an
/// operator's, so it unmounts nothing. Whatever is left is then reported by
/// `verify_no_mounts_below` rather than detached blindly.
pub(super) fn is_enclave_owned(
    entry: &crate::fsutil::MountInfoEntry,
    state_dir: Option<&Path>,
) -> bool {
    state_dir.is_some_and(|state_dir| entry.is_enclave_owned(state_dir))
}

/// The mounts at or below `root`, described and split by who created them.
///
/// Enclave's own surviving mounts are a cleanup failure. A mount Enclave did not
/// create is not Enclave's to remove, so it is reported instead of detached: the
/// operator put it there, and Enclave cannot put it back.
#[cfg(test)]
pub(crate) fn mounts_below(
    snapshot: &crate::fsutil::MountInfoSnapshot,
    root: &Path,
) -> (Vec<String>, Vec<String>) {
    let state_dir = crate::fsutil::enclave_state_root(root);
    let mut owned = Vec::new();
    let mut foreign = Vec::new();
    for entry in snapshot.at_or_below_entries(root) {
        if is_enclave_owned(entry, state_dir.as_deref()) {
            owned.push(describe_mount(entry));
        } else {
            // A foreign mount is not Enclave's to detach, so the processes
            // holding it are not an actionable part of the report; naming the
            // mount and its source is what tells the operator where to look.
            foreign.push(entry.describe());
        }
    }
    (owned, foreign)
}

pub(super) fn describe_mount(entry: &crate::fsutil::MountInfoEntry) -> String {
    let holders = mount_holders(&entry.mountpoint);
    if holders.is_empty() {
        entry.describe()
    } else {
        format!("{} (holders: {})", entry.describe(), holders.join(","))
    }
}

/// One sentence naming the mounts that survived, grouped by who created them.
pub(crate) fn remaining_mount_detail(owned: &[String], foreign: &[String]) -> String {
    let mut detail = Vec::new();
    if !owned.is_empty() {
        detail.push(format!(
            "{} Enclave mount(s) survived unmount: {}",
            owned.len(),
            owned.join("; ")
        ));
    }
    if !foreign.is_empty() {
        detail.push(format!(
            "{} mount(s) were not created by Enclave and were left in place: {}",
            foreign.len(),
            foreign.join("; ")
        ));
    }
    detail.join("; ")
}

/// Re-read mountinfo after unmounting instead of trusting `umount2` alone.
/// A mount that is still busy in another namespace leaves a live entry, and
/// deleting the workspace afterwards would leak it permanently.
pub(super) fn verify_no_mounts_below(root: &Path) -> Result<()> {
    let snapshot = crate::fsutil::MountInfoSnapshot::load()?;
    let owned = snapshot.owned_at_or_below(root);
    let foreign = snapshot.foreign_at_or_below(root);
    if !foreign.is_empty() {
        tracing::warn!(
            "{} mount(s) below {} were not created by Enclave and were left in place: {}",
            foreign.len(),
            root.display(),
            foreign.join("; ")
        );
    }
    // Only Enclave's own mounts are Enclave's failure. A foreign mount is still
    // there because someone else put it there, and detaching it would destroy
    // state Enclave did not create.
    if owned.is_empty() {
        return Ok(());
    }
    bail!(
        "{} Enclave mount(s) still present below {} after unmount: {}",
        owned.len(),
        root.display(),
        remaining_mount_detail(&owned, &foreign)
    )
}

pub(crate) fn workspace_owner_is_dead(workspace: &WorkspaceMetadata) -> bool {
    !workspace
        .runtime_pid
        .zip(workspace.runtime_starttime_ticks)
        .is_some_and(|(pid, starttime)| {
            crate::workspace::session_process_matches(pid, Some(starttime))
        })
}

#[cfg(test)]
pub(crate) fn parse_mountinfo_mountpoints(mountinfo: &str) -> Vec<std::path::PathBuf> {
    crate::fsutil::MountInfoSnapshot::parse(mountinfo).at_or_below(Path::new("/"))
}
