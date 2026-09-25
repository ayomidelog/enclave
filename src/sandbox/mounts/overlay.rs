//! The writable OverlayFS view a shared-base sandbox mounts over its cached base.

use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};

use super::super::types::SandboxMetadata;
use super::unmount::{format_errno, is_already_unmounted_errno, mount_holders, unmount_path};

/// Directory names for the writable layers of a shared-base sandbox rootfs.
const ROOTFS_UPPER_DIR: &str = "rootfs-upper";
const ROOTFS_WORK_DIR: &str = "rootfs-work";

/// Paths of the upper and work directories backing a shared-base rootfs.
pub fn rootfs_overlay_paths(metadata: &SandboxMetadata) -> Option<(PathBuf, PathBuf)> {
    metadata.rootfs_lower_path.as_ref()?;
    let sandbox_path = Path::new(&metadata.sandbox_path);
    Some((
        sandbox_path.join(ROOTFS_UPPER_DIR),
        sandbox_path.join(ROOTFS_WORK_DIR),
    ))
}

/// Mount the sandbox rootfs as an OverlayFS whose lower layer is the shared
/// cached base.
///
/// Copying a cached rootfs is metadata bound: a Debian rootfs is tens of
/// thousands of small files, and the copy costs seconds per sandbox. Mounting
/// an overlay over the shared base gives each sandbox its own writable view in
/// constant time, and writes never reach the shared base because it is only
/// ever a lower layer.
///
/// Sandboxes without a shared base keep their plain rootfs directory.
pub fn ensure_rootfs_overlay_mounted(metadata: &SandboxMetadata) -> Result<()> {
    let Some(lower) = metadata.rootfs_lower_path.as_deref() else {
        return Ok(());
    };
    let lower = Path::new(lower);
    if !lower.is_dir() {
        bail!(
            "sandbox '{}' shared rootfs base {} is missing",
            metadata.id,
            lower.display()
        );
    }
    let rootfs_path = Path::new(&metadata.rootfs_path);
    if crate::fsutil::is_mountpoint(rootfs_path)? {
        return Ok(());
    }
    let Some((upper, work)) = rootfs_overlay_paths(metadata) else {
        return Ok(());
    };
    for path in [&upper, &work, rootfs_path] {
        fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))?;
    }
    let options = format!(
        "lowerdir={},upperdir={},workdir={}",
        overlay_option_path(lower),
        overlay_option_path(&upper),
        overlay_option_path(&work)
    );
    mount_overlay(rootfs_path, &options).with_context(|| {
        format!(
            "failed to mount shared rootfs base {} for sandbox '{}'",
            lower.display(),
            metadata.id
        )
    })?;
    if !crate::fsutil::is_mountpoint(rootfs_path)? {
        bail!(
            "sandbox rootfs overlay {} is not mounted after mount_overlay",
            rootfs_path.display()
        );
    }
    Ok(())
}

/// Unmount the sandbox rootfs overlay. Sandboxes without a shared base are
/// untouched, so their plain rootfs directory survives.
pub fn unmount_rootfs_overlay(metadata: &SandboxMetadata) -> Result<()> {
    if metadata.rootfs_lower_path.is_none() {
        return Ok(());
    }
    let rootfs_path = Path::new(&metadata.rootfs_path);
    if !crate::fsutil::is_mountpoint(rootfs_path)? {
        return Ok(());
    }
    if let Err(error) = unmount_path(rootfs_path) {
        if is_already_unmounted_errno(error.raw_os_error()) {
            return Ok(());
        }
        return Err(anyhow!(
            "failed to unmount sandbox rootfs overlay mount_target={} errno={} namespace_holders={} detail={}",
            rootfs_path.display(),
            format_errno(error.raw_os_error()),
            mount_holders(&metadata.rootfs_path),
            error
        ));
    }
    Ok(())
}

fn mount_overlay(target: &Path, options: &str) -> std::io::Result<()> {
    let source = CString::new("overlay").expect("literal contains no nul");
    let filesystem = CString::new("overlay").expect("literal contains no nul");
    let target = CString::new(target.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
    let options =
        CString::new(options).map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
    let result = unsafe {
        libc::mount(
            source.as_ptr(),
            target.as_ptr(),
            filesystem.as_ptr(),
            0,
            options.as_ptr().cast(),
        )
    };
    if result == 0 {
        crate::perf::record_mount();
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Escape the characters OverlayFS uses as option separators.
pub(crate) fn overlay_option_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\134")
        .replace(',', "\\054")
        .replace(':', "\\072")
}
