//! Marking a directory that a live process is still filling in.
//!
//! Enclave creates a directory, populates it, and only then writes the metadata
//! that describes it. Repair reconciles the tree with the registry, and a
//! directory with no metadata yet is exactly what a leftover from a crashed
//! create looks like, so repair removes it. That is right for a leftover and
//! wrong for a sibling create that is still running.
//!
//! The marker is what tells them apart. It names the creating process and its
//! start time, so a marker left behind by a create that died is recognised as
//! stale rather than protecting the directory forever, and a marker that cannot
//! be parsed never claims a live owner.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Name of the marker file inside the directory being created.
pub(crate) const CREATION_MARKER_NAME: &str = ".creating";

/// Where a directory is built before it is renamed into its final place.
///
/// The staging tree is deliberately outside `sandboxes/`, which repair scans for
/// directories that carry no metadata. A staging directory inside that tree would
/// be read as an orphan, which is the race this exists to close.
pub(crate) fn creation_staging_root(state_dir: &Path) -> PathBuf {
    state_dir.join(".staging")
}

/// Create `directory` with its creation marker already inside it.
///
/// Creating the directory and then writing the marker leaves a window in which
/// the directory exists with no claim. The marker write opens a temp file, writes
/// it, and fsyncs before renaming it in, which under load is long enough for a
/// concurrent `repair_registry` — every create runs one — to read the directory as
/// a leftover and delete it out from under the create. Building the directory
/// under the staging tree and renaming it into place means the final name never
/// exists without its claim: either the rename has not happened, or the marker is
/// already there.
///
/// `kind` separates the sandbox and workspace staging trees so a sandbox id can
/// never collide with a workspace id in the staging area.
pub(crate) fn create_claimed_directory(
    state_dir: &Path,
    kind: &str,
    directory: &Path,
) -> Result<()> {
    let name = directory
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("path {} has no file name", directory.display()))?;
    let staging = creation_staging_root(state_dir).join(kind).join(name);
    if let Some(parent) = staging.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    // A staging directory left behind by a create that died is in the way of the
    // name this create wants. It cannot belong to a live create, so it goes.
    if staging.exists() && !creation_in_progress(&staging) {
        fs::remove_dir_all(&staging)
            .with_context(|| format!("failed to clear stale {}", staging.display()))?;
    }
    fs::create_dir(&staging).with_context(|| format!("failed to create {}", staging.display()))?;
    let claim = (|| {
        write_creation_marker(&staging)?;
        if let Some(parent) = directory.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        fs::rename(&staging, directory).with_context(|| {
            format!(
                "failed to move {} into place as {}",
                staging.display(),
                directory.display()
            )
        })
    })();
    if claim.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    claim
}

/// Record that this process is creating `directory`.
pub(crate) fn write_creation_marker(directory: &Path) -> Result<()> {
    let pid = std::process::id();
    let starttime = crate::workspace::session::process_starttime_ticks(pid)
        .with_context(|| format!("failed to read the start time of pid {pid}"))?;
    let path = directory.join(CREATION_MARKER_NAME);
    crate::fsutil::write_file_atomic(
        &path,
        format!("pid={pid}\nstarttime={starttime}\n").as_bytes(),
        0o600,
    )
    .with_context(|| format!("failed to write the creation marker {}", path.display()))
}

/// Remove the creation marker. A missing marker is not an error: a failed create
/// removes the whole directory, which takes the marker with it.
pub(crate) fn remove_creation_marker(directory: &Path) {
    let path = directory.join(CREATION_MARKER_NAME);
    if let Err(error) = fs::remove_file(&path) {
        if error.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!("failed to remove {}: {error:#}", path.display());
        }
    }
}

/// Whether `directory` is still being created by a live process.
pub(crate) fn creation_in_progress(directory: &Path) -> bool {
    let path = directory.join(CREATION_MARKER_NAME);
    let Ok(raw) = fs::read_to_string(&path) else {
        return false;
    };
    let mut pid = None;
    let mut starttime = None;
    for line in raw.lines() {
        if let Some(value) = line.strip_prefix("pid=") {
            pid = value.trim().parse::<u32>().ok();
        } else if let Some(value) = line.strip_prefix("starttime=") {
            starttime = value.trim().parse::<u64>().ok();
        }
    }
    let (Some(pid), Some(starttime)) = (pid, starttime) else {
        return false;
    };
    crate::workspace::session_process_matches(pid, Some(starttime))
}
