//! A workspace's storage: the disk image that backs it and the mounts it lives on.
//!
//! A quota-backed workspace gets one ext4 image, attached to a loop device and
//! mounted as its filesystem; a host-mounted workspace uses the directory the
//! user named. The submodules are split by what they do to that storage: the
//! definition checks what a workspace declared, the image creates and locates
//! the image, the mount attaches and detaches it, the unmount releases it and
//! proves it, the tmp module owns the private `/tmp` inside it, and the resize
//! module grows it.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{bail, Context, Result};

use super::types::WorkspaceMetadata;

const DISK_IMAGE_NAME: &str = "fs.img";

/// Directory on the workspace filesystem that backs the workspace's `/tmp`.
///
/// The name is hidden on purpose. The workspace filesystem root is mounted at
/// `/home`, so any directory in it is reachable from inside the workspace; a
/// plain `tmp` entry there was the same directory as the `/tmp` mount, and
/// removing it unlinked the live mount source, leaving every later `/tmp` write
/// failing with `ENOENT`. A dot-prefixed name keeps the backing directory out of
/// the workspace's home view for ordinary cleanup commands and makes the
/// aliasing impossible to trigger by name.
pub(crate) const WORKSPACE_TMP_DIR: &str = ".enclave-tmp";

/// Backing directory that older workspaces used for the `/tmp` mount.
pub(crate) const LEGACY_WORKSPACE_TMP_DIR: &str = "tmp";
/// How long to wait between checks that the kernel released a loop device.
///
/// The wait is the kernel's, not this interval's: an autoclear device is released
/// 22 to 48 ms after the unmount on this host, and detaching it explicitly with
/// `losetup -d` is not faster (45 ms median against 35 ms). The interval only
/// decides how late the release is noticed.
///
/// Ten milliseconds is the right granularity because the check is not free: finding
/// the device that backs one image means reading `backing_file` for every loop device
/// on the host, and with 53 of them that is 1.1 ms a scan. A shorter interval would
/// spend more time scanning than it saves in latency, and a longer one would add its
/// own delay to every stop. Ten milliseconds still costs far less than the `losetup`
/// fork this replaced, which was about 10 ms for the same answer.
const LOOP_DETACH_POLL_INTERVAL: Duration = Duration::from_millis(10);
static DISK_BACKEND_CHECK: OnceLock<Result<(), String>> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkspaceDiskResize {
    pub previous_bytes: u64,
    pub new_bytes: u64,
}

mod definition;
mod ext4;
mod image;
mod mount;
mod resize;
mod tmp;
mod unmount;

pub use definition::{validate_workspace_storage_limits, verify_workspace_source};
pub use mount::{create_workspace_storage, ensure_workspace_storage_ready};
pub use resize::increase_workspace_disk_allocation;
pub use unmount::ensure_workspace_storage_unmounted;

// Used by workspace lifecycle modules and their tests.
pub(crate) use image::{workspace_disk_image_path, workspace_uses_disk_image};

/// Whether this host has the tools a workspace disk quota needs.
///
/// The capability report asks this rather than calling the internal check, so the
/// question it is asking is visible where it is asked.
pub(crate) fn disk_backend_available() -> anyhow::Result<()> {
    image::ensure_disk_backend_available()
}
pub(crate) use mount::with_workspace_storage_mounted;
#[cfg(test)]
pub(crate) use tmp::ensure_workspace_tmp_layout;
pub(crate) use tmp::reset_workspace_tmp;
pub(crate) use tmp::workspace_tmp_path;
pub(crate) use unmount::ensure_workspace_storage_unmounted_many;
pub(crate) use unmount::loop_device_is_mounted;
pub(crate) use unmount::loop_devices_for_image;
#[cfg(test)]
pub(crate) use unmount::mount_holders;
#[cfg(test)]
pub(crate) use unmount::mounts_below;
#[cfg(test)]
pub(crate) use unmount::remaining_mount_detail;
pub(crate) use unmount::unmount_mounts_at_or_below_excluding;
pub(crate) use unmount::verify_disk_image_loop_detached;

pub(crate) use mount::root_overlay_paths;

#[cfg(test)]
pub(crate) use definition::MIN_DISK_BYTES;

// Entry points the storage tests reach through `super::`.
#[cfg(test)]
pub(crate) use tmp::{clear_workspace_tmp_contents, reset_mounted_workspace_tmp};
#[cfg(test)]
pub(crate) use unmount::{
    parse_losetup_for_image, parse_mountinfo_mountpoints, unmount_error, workspace_owner_is_dead,
};

#[cfg(test)]
#[path = "../../../tests/src/workspace/storage/mod.rs"]
mod tests;
