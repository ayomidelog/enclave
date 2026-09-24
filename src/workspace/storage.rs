use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use super::types::WorkspaceMetadata;

const DISK_IMAGE_NAME: &str = "fs.img";
const MIN_DISK_BYTES: u64 = 32 * 1024 * 1024;
const LOOP_DETACH_TIMEOUT: Duration = Duration::from_secs(2);
const LOOP_DETACH_POLL_INTERVAL: Duration = Duration::from_millis(50);
static DISK_BACKEND_CHECK: OnceLock<Result<(), String>> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkspaceDiskResize {
    pub previous_bytes: u64,
    pub new_bytes: u64,
}

pub fn validate_workspace_storage_limits(
    home_mount_source_path: Option<&str>,
    disk_bytes: Option<u64>,
) -> Result<()> {
    if let Some(bytes) = disk_bytes {
        if bytes < MIN_DISK_BYTES {
            bail!(
                "workspace disk quota must be at least {} MiB",
                MIN_DISK_BYTES / (1024 * 1024)
            );
        }
        if home_mount_source_path.is_some() {
            bail!(
                "workspace disk quota is not supported with workspace_dir/path host mounts; use the default Enclave-managed workspace storage"
            );
        }
    }
    Ok(())
}

pub fn create_workspace_storage(workspace: &WorkspaceMetadata) -> Result<()> {
    if workspace_uses_disk_image(workspace) {
        ensure_disk_backend_available()?;
        initialize_disk_image(workspace)?;
    }
    Ok(())
}

pub fn ensure_workspace_storage_ready(workspace: &WorkspaceMetadata) -> Result<()> {
    if workspace_uses_disk_image(workspace) {
        ensure_disk_backend_available()?;
        initialize_disk_image(workspace)?;
        mount_disk_image_if_needed(workspace)?;
        ensure_root_overlay_layout(workspace)?;
    }
    Ok(())
}

pub(crate) fn root_overlay_paths(
    workspace: &WorkspaceMetadata,
) -> Option<(PathBuf, PathBuf, PathBuf)> {
    if !workspace_uses_disk_image(workspace) {
        return None;
    }
    Some((
        PathBuf::from(&workspace.filesystem_path).join("root-upper"),
        PathBuf::from(&workspace.filesystem_path).join("root-work"),
        PathBuf::from(&workspace.workspace_path).join("root-merged"),
    ))
}

fn ensure_root_overlay_layout(workspace: &WorkspaceMetadata) -> Result<()> {
    let Some((upper, work, merged)) = root_overlay_paths(workspace) else {
        return Ok(());
    };
    for path in [&upper, &work, &merged] {
        fs::create_dir_all(path).with_context(|| {
            format!("failed to create root overlay directory {}", path.display())
        })?;
    }
    Ok(())
}

pub fn ensure_workspace_storage_unmounted(workspace: &WorkspaceMetadata) -> Result<()> {
    let workspace_root = Path::new(&workspace.workspace_path);
    let snapshot = crate::fsutil::MountInfoSnapshot::load()?;
    let mountpoints = snapshot.at_or_below(workspace_root);

    let owner_is_dead = workspace_owner_is_dead(workspace);
    for path in mountpoints {
        unmount_workspace_path(&path, owner_is_dead)?;
    }
    verify_no_mounts_below(workspace_root)?;
    verify_disk_image_loop_detached(workspace)?;
    Ok(())
}

/// Re-read mountinfo after unmounting instead of trusting `umount2` alone.
/// A mount that is still busy in another namespace leaves a live entry, and
/// deleting the workspace afterwards would leak it permanently.
fn verify_no_mounts_below(root: &Path) -> Result<()> {
    let remaining = crate::fsutil::MountInfoSnapshot::load()?.at_or_below(root);
    if remaining.is_empty() {
        return Ok(());
    }
    let holders = remaining
        .iter()
        .map(|path| {
            let holders = mount_holders(path);
            if holders.is_empty() {
                path.display().to_string()
            } else {
                format!("{} (holders: {})", path.display(), holders.join(","))
            }
        })
        .collect::<Vec<_>>()
        .join("; ");
    bail!(
        "{} workspace mount(s) still present below {} after unmount: {}",
        remaining.len(),
        root.display(),
        holders
    )
}

/// Unmount all workspace storage using one mountinfo snapshot. This avoids a
/// full `/proc/self/mountinfo` scan for every workspace during sandbox stop.
pub(crate) fn ensure_workspace_storage_unmounted_many(
    workspaces: &[WorkspaceMetadata],
) -> Result<()> {
    let snapshot = crate::fsutil::MountInfoSnapshot::load()?;
    let mut mountpoints = Vec::new();
    for workspace in workspaces {
        let root = Path::new(&workspace.workspace_path);
        let owner_is_dead = workspace_owner_is_dead(workspace);
        for path in snapshot.at_or_below(root) {
            mountpoints.push((path, owner_is_dead));
        }
    }
    mountpoints.sort_by(|left, right| {
        right
            .0
            .components()
            .count()
            .cmp(&left.0.components().count())
    });
    mountpoints.dedup_by(|left, right| left.0 == right.0);
    for (path, owner_is_dead) in mountpoints {
        unmount_workspace_path(&path, owner_is_dead)?;
    }
    let snapshot = crate::fsutil::MountInfoSnapshot::load()?;
    for workspace in workspaces {
        let remaining = snapshot.at_or_below(Path::new(&workspace.workspace_path));
        if !remaining.is_empty() {
            bail!(
                "workspace '{}' still has {} mount(s) below {} after unmount: {}",
                workspace.id,
                remaining.len(),
                workspace.workspace_path,
                remaining
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
    for workspace in workspaces {
        verify_disk_image_loop_detached(workspace)?;
    }
    Ok(())
}

pub(crate) fn reset_workspace_tmp(workspace: &WorkspaceMetadata) -> Result<()> {
    if !workspace_uses_disk_image(workspace) {
        return Ok(());
    }
    // `/tmp` for a quota-backed workspace lives inside the workspace disk
    // image. Clearing the host mountpoint after the image is unmounted would
    // leave the image untouched, so mount the image when the caller is not
    // already holding it.
    with_workspace_storage_mounted(workspace, || reset_mounted_workspace_tmp(workspace))
}

fn reset_mounted_workspace_tmp(workspace: &WorkspaceMetadata) -> Result<()> {
    let workspace_root = PathBuf::from(&workspace.filesystem_path);
    if !crate::fsutil::is_mountpoint(&workspace_root)? {
        bail!(
            "refusing to reset workspace /tmp: {} is not mounted",
            workspace_root.display()
        );
    }
    clear_workspace_tmp_contents(&workspace_root)
}

/// Remove every entry under `<workspace filesystem>/tmp`, keeping the directory
/// itself. The caller owns the storage mount; this only touches paths.
fn clear_workspace_tmp_contents(workspace_root: &Path) -> Result<()> {
    let tmp_path = workspace_root.join("tmp");
    let canonical_root = fs::canonicalize(workspace_root).with_context(|| {
        format!(
            "failed to resolve workspace filesystem {}",
            workspace_root.display()
        )
    })?;
    let canonical_tmp =
        crate::fsutil::ensure_path_within(&canonical_root, &tmp_path, "workspace tmp")?;
    let metadata = match fs::symlink_metadata(&canonical_tmp) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(&canonical_tmp).with_context(|| {
                format!("failed to create workspace tmp {}", canonical_tmp.display())
            })?;
            return Ok(());
        }
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed to inspect workspace tmp {}",
                    canonical_tmp.display()
                )
            })
        }
    };
    if metadata.file_type().is_symlink() {
        bail!(
            "workspace tmp path must not be a symlink: {}",
            canonical_tmp.display()
        );
    }

    fs::create_dir_all(&canonical_tmp)
        .with_context(|| format!("failed to create workspace tmp {}", canonical_tmp.display()))?;
    for entry in fs::read_dir(&canonical_tmp)
        .with_context(|| format!("failed to read workspace tmp {}", canonical_tmp.display()))?
    {
        let path = entry
            .with_context(|| {
                format!(
                    "failed to inspect workspace tmp {}",
                    canonical_tmp.display()
                )
            })?
            .path();
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("failed to inspect workspace tmp entry {}", path.display()))?;
        if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
            fs::remove_dir_all(&path).with_context(|| {
                format!(
                    "failed to remove workspace tmp directory {}",
                    path.display()
                )
            })?;
        } else {
            fs::remove_file(&path).with_context(|| {
                format!("failed to remove workspace tmp entry {}", path.display())
            })?;
        }
    }
    Ok(())
}

pub(crate) fn unmount_mounts_at_or_below_excluding(
    root: &Path,
    excluded_roots: &[PathBuf],
) -> Result<usize> {
    let mountpoints = crate::fsutil::MountInfoSnapshot::load()?.at_or_below(root);
    let mut unmounted = 0usize;
    for path in mountpoints {
        if excluded_roots
            .iter()
            .any(|excluded| path.starts_with(excluded))
        {
            continue;
        }
        unmount_workspace_path(&path, true)?;
        unmounted += 1;
    }
    Ok(unmounted)
}

fn workspace_owner_is_dead(workspace: &WorkspaceMetadata) -> bool {
    !workspace
        .runtime_pid
        .zip(workspace.runtime_starttime_ticks)
        .is_some_and(|(pid, starttime)| {
            crate::workspace::session_process_matches(pid, Some(starttime))
        })
}

fn verify_disk_image_loop_detached(workspace: &WorkspaceMetadata) -> Result<()> {
    if !workspace_uses_disk_image(workspace) {
        return Ok(());
    }
    let image = workspace_disk_image_path(workspace);
    // The kernel releases an autoclear loop device asynchronously once the last
    // mount reference is gone, so poll briefly instead of failing a stop that is
    // still tearing down.
    let deadline = Instant::now() + LOOP_DETACH_TIMEOUT;
    let mut devices = loop_devices_for_image(&image)?;
    while !devices.is_empty() && Instant::now() < deadline {
        thread::sleep(LOOP_DETACH_POLL_INTERVAL);
        devices = loop_devices_for_image(&image)?;
    }
    if devices.is_empty() {
        return Ok(());
    }

    // A loop device can outlive the unmount when something outside Enclave's
    // mount ownership still holds a file open on it, for example a client
    // process with a descriptor into the workspace filesystem. That device is
    // released when the last holder closes, so only a surviving mount is a
    // cleanup failure Enclave can act on.
    let holders = namespaces_mounting_devices(&devices);
    if holders.is_empty() {
        tracing::warn!(
            "workspace image {} is still attached to {} but no mount references it; \
             the kernel will release the device when the last open file closes",
            image.display(),
            devices.join(", ")
        );
        return Ok(());
    }
    bail!(
        "workspace image {} remains attached to loop device(s) {} and is still mounted in {}",
        image.display(),
        devices.join(", "),
        holders.join(", ")
    )
}

fn loop_devices_for_image(image: &Path) -> Result<Vec<String>> {
    let output = Command::new("losetup")
        .args(["-j"])
        .arg(image)
        .output()
        .with_context(|| format!("failed to inspect loop devices for {}", image.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "losetup could not inspect workspace image {} ({}): {}",
            image.display(),
            output.status,
            stderr.trim()
        );
    }
    Ok(parse_loop_devices(&String::from_utf8_lossy(&output.stdout)))
}

/// Find mount namespaces that still mount one of the given devices. This turns
/// an opaque busy loop device into an actionable holder.
fn namespaces_mounting_devices(devices: &[String]) -> Vec<String> {
    let Ok(entries) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut holders = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .filter(|pid| !pid.is_empty() && pid.bytes().all(|byte| byte.is_ascii_digit()))
        else {
            continue;
        };
        let mountinfo = format!("/proc/{pid}/mountinfo");
        let Ok(raw) = fs::read_to_string(&mountinfo) else {
            continue;
        };
        let mounted = raw.lines().any(|line| {
            line.split(" - ")
                .nth(1)
                .and_then(|rest| rest.split_whitespace().nth(1))
                .is_some_and(|source| devices.iter().any(|device| device == source))
        });
        if mounted {
            holders.push(format!("pid {pid}"));
        }
    }
    holders
}

fn parse_loop_devices(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| {
            line.split_once(":")
                .map(|(device, _)| device.trim().to_string())
        })
        .filter(|device| {
            device.strip_prefix("/dev/loop").is_some_and(|suffix| {
                !suffix.is_empty() && suffix.chars().all(|ch| ch.is_ascii_digit())
            })
        })
        .collect()
}

#[cfg(test)]
fn parse_mountinfo_mountpoints(mountinfo: &str) -> Vec<PathBuf> {
    crate::fsutil::MountInfoSnapshot::parse(mountinfo).at_or_below(Path::new("/"))
}

fn unmount_workspace_path(path: &Path, owner_is_dead: bool) -> Result<()> {
    match unmount_path(path, 0) {
        Ok(()) => Ok(()),
        Err(error) if is_already_unmounted_errno(error.raw_os_error()) => Ok(()),
        Err(_) if owner_is_dead => {
            crate::perf::record_cleanup_retry();
            match unmount_path(path, libc::MNT_DETACH) {
                Ok(()) => Ok(()),
                Err(error) if is_already_unmounted_errno(error.raw_os_error()) => Ok(()),
                Err(error) => Err(unmount_error(path, &error)),
            }
        }
        Err(error) => Err(unmount_error(path, &error)),
    }
}

fn unmount_path(path: &Path, flags: i32) -> std::io::Result<()> {
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
    let result = unsafe { libc::umount2(path.as_ptr(), flags) };
    if result == 0 {
        crate::perf::record_unmount();
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn unmount_error(path: &Path, error: &std::io::Error) -> anyhow::Error {
    let holders = mount_holders(path);
    anyhow::anyhow!(
        "failed to unmount workspace mount target={} errno={} namespace_holders={} detail={}",
        path.display(),
        format_errno(error.raw_os_error()),
        if holders.is_empty() {
            "none".to_string()
        } else {
            holders.join(",")
        },
        error
    )
}

fn is_already_unmounted_errno(errno: Option<i32>) -> bool {
    matches!(errno, Some(libc::ENOENT | libc::EINVAL))
}

fn mount_holders(path: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut holders = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .filter(|value| value.chars().all(|ch| ch.is_ascii_digit()))
        else {
            continue;
        };
        let mountinfo_path = entry.path().join("mountinfo");
        let Ok(mountinfo) = fs::read_to_string(mountinfo_path) else {
            continue;
        };
        if !crate::fsutil::MountInfoSnapshot::parse(&mountinfo).contains(path) {
            continue;
        }
        let namespace = fs::read_link(entry.path().join("ns/mnt"))
            .map(|value| value.to_string_lossy().to_string())
            .unwrap_or_else(|_| "unknown".to_string());
        holders.push(format!("pid={pid}@{namespace}"));
        if holders.len() == 8 {
            break;
        }
    }
    holders
}

fn format_errno(errno: Option<i32>) -> String {
    match errno {
        Some(libc::EBUSY) => "EBUSY(16)".to_string(),
        Some(libc::ENOENT) => "ENOENT(2)".to_string(),
        Some(libc::EINVAL) => "EINVAL(22)".to_string(),
        Some(libc::EPERM) => "EPERM(1)".to_string(),
        Some(value) => format!("errno({value})"),
        None => "unknown".to_string(),
    }
}

pub fn increase_workspace_disk_allocation(
    workspace: &WorkspaceMetadata,
    new_disk_bytes: u64,
) -> Result<WorkspaceDiskResize> {
    let current_disk_bytes = workspace.limits.disk_bytes.ok_or_else(|| {
        anyhow::anyhow!(
            "workspace '{}' has no Enclave-managed disk allocation",
            workspace.id
        )
    })?;

    if workspace.home_mount_source_path.is_some() {
        bail!(
            "workspace '{}' uses a host-backed workspace directory; disk allocation resize is only supported for Enclave-managed storage",
            workspace.id
        );
    }
    if new_disk_bytes < MIN_DISK_BYTES {
        bail!(
            "workspace disk allocation must be at least {} MiB",
            MIN_DISK_BYTES / (1024 * 1024)
        );
    }
    if new_disk_bytes < current_disk_bytes {
        bail!(
            "workspace disk resize only supports increases; requested {} bytes is below the current {} bytes",
            new_disk_bytes,
            current_disk_bytes
        );
    }
    if new_disk_bytes == current_disk_bytes {
        return Ok(WorkspaceDiskResize {
            previous_bytes: current_disk_bytes,
            new_bytes: current_disk_bytes,
        });
    }

    ensure_disk_backend_available()?;
    let image = workspace_disk_image_path(workspace);
    let image_metadata = fs::metadata(&image)
        .with_context(|| format!("failed to inspect workspace disk image {}", image.display()))?;
    if !image_metadata.is_file() {
        bail!(
            "workspace disk image {} is not a regular file",
            image.display()
        );
    }
    if image_metadata.len() < current_disk_bytes {
        bail!(
            "workspace disk image {} is smaller than its recorded allocation",
            image.display()
        );
    }
    if image_metadata.len() > new_disk_bytes {
        bail!(
            "workspace disk image {} is already larger than the requested allocation; refusing to shrink it",
            image.display()
        );
    }
    if crate::fsutil::is_mountpoint(Path::new(&workspace.filesystem_path))? {
        bail!(
            "workspace disk image {} is still mounted; stop the workspace and retry",
            image.display()
        );
    }

    let check = Command::new("e2fsck")
        .args(["-p"])
        .arg(&image)
        .output()
        .with_context(|| {
            format!(
                "failed to check workspace ext4 filesystem {}",
                image.display()
            )
        })?;
    if !matches!(check.status.code(), Some(0 | 1)) {
        let stderr = String::from_utf8_lossy(&check.stderr);
        bail!(
            "failed to check workspace ext4 filesystem {} ({}): {}",
            image.display(),
            check.status,
            stderr.trim()
        );
    }

    let truncate = Command::new("truncate")
        .args(["-s", &new_disk_bytes.to_string()])
        .arg(&image)
        .output()
        .with_context(|| format!("failed to grow workspace disk image {}", image.display()))?;
    if !truncate.status.success() {
        let stderr = String::from_utf8_lossy(&truncate.stderr);
        bail!(
            "failed to grow workspace disk image {} ({}): {}",
            image.display(),
            truncate.status,
            stderr.trim()
        );
    }

    let resize = Command::new("resize2fs")
        .arg(&image)
        .output()
        .with_context(|| format!("failed to grow ext4 filesystem {}", image.display()))?;
    if !resize.status.success() {
        let stderr = String::from_utf8_lossy(&resize.stderr);
        bail!(
            "grew workspace disk image {} but failed to grow its ext4 filesystem ({}): {}",
            image.display(),
            resize.status,
            stderr.trim()
        );
    }

    let final_size = fs::metadata(&image)
        .with_context(|| format!("failed to verify workspace disk image {}", image.display()))?
        .len();
    if final_size < new_disk_bytes {
        bail!(
            "workspace disk image {} is smaller than the requested allocation after resize",
            image.display()
        );
    }

    Ok(WorkspaceDiskResize {
        previous_bytes: current_disk_bytes,
        new_bytes: new_disk_bytes,
    })
}

pub fn with_workspace_storage_mounted<T, F>(
    workspace: &WorkspaceMetadata,
    operation: F,
) -> Result<T>
where
    F: FnOnce() -> Result<T>,
{
    if !workspace_uses_disk_image(workspace) {
        return operation();
    }

    let mountpoint = Path::new(&workspace.filesystem_path);
    let was_mounted = crate::fsutil::is_mountpoint(mountpoint)?;
    if !was_mounted {
        ensure_workspace_storage_ready(workspace)?;
    }

    let result = operation();

    if !was_mounted {
        let _ = ensure_workspace_storage_unmounted(workspace);
    }

    result
}

fn mount_disk_image_if_needed(workspace: &WorkspaceMetadata) -> Result<()> {
    let mountpoint = Path::new(&workspace.filesystem_path);
    if crate::fsutil::is_mountpoint(mountpoint)? {
        return Ok(());
    }
    fs::create_dir_all(mountpoint)
        .with_context(|| format!("failed to create {}", mountpoint.display()))?;
    let image = workspace_disk_image_path(workspace);
    let output = Command::new("mount")
        .args(["-o", "loop"])
        .arg(&image)
        .arg(mountpoint)
        .output()
        .with_context(|| {
            format!(
                "failed to mount quota-backed workspace image {} on {}",
                image.display(),
                mountpoint.display()
            )
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "failed to mount quota-backed workspace image {} on {} ({}): {}",
            image.display(),
            mountpoint.display(),
            output.status,
            stderr.trim()
        );
    }
    Ok(())
}

fn initialize_disk_image(workspace: &WorkspaceMetadata) -> Result<()> {
    let image = workspace_disk_image_path(workspace);
    if image.exists() {
        return Ok(());
    }
    let disk_bytes = workspace.limits.disk_bytes.ok_or_else(|| {
        anyhow::anyhow!("workspace '{}' has no disk quota configured", workspace.id)
    })?;
    if let Some(parent) = image.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let truncate = Command::new("truncate")
        .args(["-s", &disk_bytes.to_string()])
        .arg(&image)
        .output()
        .with_context(|| format!("failed to create sparse disk image {}", image.display()))?;
    if !truncate.status.success() {
        let stderr = String::from_utf8_lossy(&truncate.stderr);
        bail!(
            "failed to create sparse disk image {} ({}): {}",
            image.display(),
            truncate.status,
            stderr.trim()
        );
    }
    let mkfs = Command::new("mkfs.ext4")
        .args(["-F", "-q"])
        .arg(&image)
        .output()
        .with_context(|| format!("failed to format ext4 disk image {}", image.display()))?;
    if !mkfs.status.success() {
        let stderr = String::from_utf8_lossy(&mkfs.stderr);
        bail!(
            "failed to format ext4 disk image {} ({}): {}",
            image.display(),
            mkfs.status,
            stderr.trim()
        );
    }
    Ok(())
}

fn ensure_disk_backend_available() -> Result<()> {
    let result = DISK_BACKEND_CHECK.get_or_init(|| {
        for command in [
            "truncate",
            "mkfs.ext4",
            "resize2fs",
            "e2fsck",
            "mount",
            "losetup",
        ] {
            let status = Command::new("sh")
                .args(["-c", &format!("command -v {command} >/dev/null 2>&1")])
                .status()
                .map_err(|error| format!("failed to probe availability of {command}: {error}"))?;
            if !status.success() {
                return Err(format!(
                    "workspace disk quota requires '{}' to be available on the host",
                    command
                ));
            }
        }
        Ok(())
    });
    result
        .as_ref()
        .map_err(|error| anyhow::anyhow!(error))
        .copied()
}

pub(crate) fn workspace_disk_image_path(workspace: &WorkspaceMetadata) -> PathBuf {
    PathBuf::from(&workspace.workspace_path).join(DISK_IMAGE_NAME)
}

pub(crate) fn workspace_uses_disk_image(workspace: &WorkspaceMetadata) -> bool {
    workspace.limits.disk_bytes.is_some() && workspace.home_mount_source_path.is_none()
}

#[cfg(test)]
#[path = "../../tests/src/workspace/storage.rs"]
mod tests;
