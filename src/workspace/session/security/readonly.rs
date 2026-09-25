//! Making the workspace namespace's own mounts immutable, and detaching the old
//! root after the pivot.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use nix::mount::{mount, umount2, MntFlags, MsFlags};

/// Remount `/proc/sys`, `/sys`, and `/sys/fs/cgroup` read-only.
///
/// The workspace receives these from the host, and a writable `/proc/sys` would
/// let it change host kernel settings while a writable `/sys` would let it reach
/// the device and module trees. A path that is not present in the namespace is
/// skipped rather than treated as an error.
pub(crate) fn tighten_namespace_mounts() -> Result<()> {
    if Path::new("/proc/sys").exists() {
        remount_read_only_with_policy(Path::new("/proc/sys"))?;
    }
    if Path::new("/sys").exists() {
        remount_read_only_with_policy(Path::new("/sys"))?;
    }
    if Path::new("/sys/fs/cgroup").exists() {
        remount_read_only_with_policy(Path::new("/sys/fs/cgroup"))?;
    }
    Ok(())
}

/// Detach and remove the directory the session started in, after the pivot.
///
/// The detach is lazy (`MNT_DETACH`) because the session still holds the old root
/// as its working directory and as the source of the mounts it has already made;
/// a lazy detach releases it once the last reference goes away.
pub(crate) fn detach_old_root(old_root: &Path) -> Result<()> {
    if !old_root.exists() {
        return Ok(());
    }
    umount2(old_root, MntFlags::MNT_DETACH)
        .with_context(|| format!("failed to detach old root {}", old_root.display()))?;
    if let Err(err) = fs::remove_dir(old_root) {
        if err.kind() != std::io::ErrorKind::NotFound {
            return Err(err)
                .with_context(|| format!("failed to remove old root {}", old_root.display()));
        }
    }
    Ok(())
}

/// Bind `path` onto itself recursively, then remount the whole tree read-only.
fn bind_remount_read_only(path: &Path) -> Result<()> {
    mount(
        Some(path),
        path,
        Option::<&str>::None,
        MsFlags::MS_BIND | MsFlags::MS_REC,
        Option::<&str>::None,
    )
    .with_context(|| format!("failed to bind-mount {}", path.display()))?;
    mount(
        Option::<&str>::None,
        path,
        Option::<&str>::None,
        MsFlags::MS_BIND | MsFlags::MS_REMOUNT | MsFlags::MS_RDONLY | MsFlags::MS_REC,
        Option::<&str>::None,
    )
    .with_context(|| format!("failed to remount {} read-only", path.display()))?;
    Ok(())
}

/// Remount `path` read-only, tolerating the one failure that is not actionable.
fn remount_read_only_with_policy(path: &Path) -> Result<()> {
    if let Err(err) = bind_remount_read_only(path) {
        if should_ignore_readonly_remount_error(path, &err) {
            tracing::warn!(
                "workspace runtime could not remount {} read-only; continuing with remaining hardening: {err:#}",
                path.display()
            );
            return Ok(());
        }
        return Err(err).with_context(|| format!("failed to remount {} read-only", path.display()));
    }
    Ok(())
}

/// Whether a failed read-only remount of `path` is expected on a host that does
/// not permit it.
///
/// `/sys` and `/sys/fs/cgroup` are already read-only on many hosts, and the
/// kernel then refuses the bind remount with `EPERM`. That is the state the
/// remount was trying to reach, so it is not a hardening failure. `/proc/sys` is
/// held to the same standard as everything else: a failure there is real and
/// must be reported, because the alternative is a writable host sysctl tree
/// inside the workspace.
pub(super) fn should_ignore_readonly_remount_error(path: &Path, err: &anyhow::Error) -> bool {
    matches!(path.to_str(), Some("/sys") | Some("/sys/fs/cgroup"))
        && error_has_errno(err, libc::EPERM)
}

fn error_has_errno(err: &anyhow::Error, errno: i32) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .and_then(|io_err| io_err.raw_os_error())
            == Some(errno)
            || cause
                .downcast_ref::<nix::errno::Errno>()
                .map(|nix_err| *nix_err as i32)
                == Some(errno)
    })
}
