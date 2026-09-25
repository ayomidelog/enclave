use super::*;

pub fn ensure_workspace_storage_unmounted(workspace: &WorkspaceMetadata) -> Result<()> {
    let workspace_root = Path::new(&workspace.workspace_path);
    let snapshot = crate::fsutil::MountInfoSnapshot::load()?;
    let owner_is_dead = workspace_owner_is_dead(workspace);
    let state_dir = crate::fsutil::enclave_state_root(workspace_root);
    for entry in snapshot.at_or_below_entries(workspace_root) {
        if !is_enclave_owned(entry, state_dir.as_deref()) {
            continue;
        }
        unmount_workspace_path(&entry.mountpoint, owner_is_dead)?;
    }
    verify_no_mounts_below(workspace_root)?;
    verify_disk_image_loop_detached(workspace)?;
    Ok(())
}

/// Whether Enclave may unmount this mount.
///
/// Without a provable state directory Enclave cannot tell its own mounts from an
/// operator's, so it unmounts nothing. Whatever is left is then reported by
/// `verify_no_mounts_below` rather than detached blindly.
fn is_enclave_owned(entry: &crate::fsutil::MountInfoEntry, state_dir: Option<&Path>) -> bool {
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

fn describe_mount(entry: &crate::fsutil::MountInfoEntry) -> String {
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
pub(crate) fn verify_no_mounts_below(root: &Path) -> Result<()> {
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

/// Unmount all workspace storage using one mountinfo snapshot. This avoids a
/// full `/proc/self/mountinfo` scan for every workspace during sandbox stop.
pub(crate) fn ensure_workspace_storage_unmounted_many(
    workspaces: &[WorkspaceMetadata],
) -> Result<()> {
    let snapshot = crate::fsutil::MountInfoSnapshot::load()?;
    let mut mountpoints = Vec::new();
    for workspace in workspaces {
        let root = Path::new(&workspace.workspace_path);
        let state_dir = crate::fsutil::enclave_state_root(root);
        let owner_is_dead = workspace_owner_is_dead(workspace);
        for entry in snapshot.at_or_below_entries(root) {
            if !is_enclave_owned(entry, state_dir.as_deref()) {
                continue;
            }
            mountpoints.push((entry.mountpoint.clone(), owner_is_dead));
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
        let root = Path::new(&workspace.workspace_path);
        let owned = snapshot.owned_at_or_below(root);
        let foreign = snapshot.foreign_at_or_below(root);
        if !foreign.is_empty() {
            tracing::warn!(
                "workspace '{}': {} mount(s) below {} were not created by Enclave and were left in place: {}",
                workspace.id,
                foreign.len(),
                workspace.workspace_path,
                foreign.join("; ")
            );
        }
        if !owned.is_empty() {
            bail!(
                "workspace '{}' still has {} Enclave mount(s) below {} after unmount: {}",
                workspace.id,
                owned.len(),
                workspace.workspace_path,
                remaining_mount_detail(&owned, &foreign)
            );
        }
    }
    for workspace in workspaces {
        verify_disk_image_loop_detached(workspace)?;
    }
    Ok(())
}

/// What a sweep of the sandboxes tree for mounts a previous run left behind found.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct StaleMountSweep {
    /// Enclave's own mounts that were unmounted.
    pub unmounted: usize,
    /// Mounts under the tree that Enclave did not create, described for a report.
    pub foreign: Vec<String>,
}

pub(crate) fn unmount_mounts_at_or_below_excluding(
    root: &Path,
    excluded_roots: &[PathBuf],
) -> Result<StaleMountSweep> {
    let snapshot = crate::fsutil::MountInfoSnapshot::load()?;
    let state_dir = crate::fsutil::enclave_state_root(root);
    let mut sweep = StaleMountSweep::default();
    for entry in snapshot.at_or_below_entries(root) {
        if excluded_roots
            .iter()
            .any(|excluded| entry.mountpoint.starts_with(excluded))
        {
            continue;
        }
        if !is_enclave_owned(entry, state_dir.as_deref()) {
            sweep.foreign.push(describe_mount(entry));
            continue;
        }
        unmount_workspace_path(&entry.mountpoint, true)?;
        sweep.unmounted += 1;
    }
    Ok(sweep)
}

pub(crate) fn workspace_owner_is_dead(workspace: &WorkspaceMetadata) -> bool {
    !workspace
        .runtime_pid
        .zip(workspace.runtime_starttime_ticks)
        .is_some_and(|(pid, starttime)| {
            crate::workspace::session_process_matches(pid, Some(starttime))
        })
}

pub(crate) fn verify_disk_image_loop_detached(workspace: &WorkspaceMetadata) -> Result<()> {
    if !workspace_uses_disk_image(workspace) {
        return Ok(());
    }
    let image = workspace_disk_image_path(workspace);
    // The kernel releases an autoclear loop device asynchronously once the last
    // mount reference is gone, so poll briefly instead of failing a stop that is
    // still tearing down.
    let loop_detach = crate::deadlines::loop_detach();
    let deadline = Instant::now() + loop_detach.get();
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
        "workspace image {} remains attached to loop device(s) {} and is still mounted in {} after waiting {}",
        image.display(),
        devices.join(", "),
        holders.join(", "),
        loop_detach.describe_timeout()
    )
}

/// Loop devices backing `image`.
///
/// This reads sysfs instead of spawning `losetup`, which matters because the
/// stop path polls it until the kernel releases an autoclear device.
pub(crate) fn loop_devices_for_image(image: &Path) -> Result<Vec<String>> {
    crate::fsutil::loop_devices_for_image(image)
}

/// Whether some mount namespace still mounts `device`.
///
/// This is the question `verify_disk_image_loop_detached` asks before it decides
/// that an attached device is Enclave's failure, and the resource inventory asks
/// it too. Sharing the predicate keeps a certificate's `loop_device_absent` flag
/// and its inventory diff from disagreeing about the same device.
///
/// An attached device that no namespace mounts is not a failure either can act
/// on: the kernel detaches an autoclear device once its last holder closes, and
/// that holder can be a process outside Enclave.
pub(crate) fn loop_device_is_mounted(device: &str) -> bool {
    !namespaces_mounting_devices(&[device.to_string()]).is_empty()
}

#[cfg(test)]
pub(crate) use crate::fsutil::parse_losetup_for_image;

/// Find mount namespaces that still mount one of the given devices. This turns
/// an opaque busy loop device into an actionable holder.
pub(crate) fn namespaces_mounting_devices(devices: &[String]) -> Vec<String> {
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

#[cfg(test)]
pub(crate) fn parse_mountinfo_mountpoints(mountinfo: &str) -> Vec<PathBuf> {
    crate::fsutil::MountInfoSnapshot::parse(mountinfo).at_or_below(Path::new("/"))
}

pub(crate) fn unmount_workspace_path(path: &Path, owner_is_dead: bool) -> Result<()> {
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

pub(crate) fn unmount_path(path: &Path, flags: i32) -> std::io::Result<()> {
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

pub(crate) fn unmount_error(path: &Path, error: &std::io::Error) -> anyhow::Error {
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

pub(crate) fn is_already_unmounted_errno(errno: Option<i32>) -> bool {
    matches!(errno, Some(libc::ENOENT | libc::EINVAL))
}

pub(crate) fn mount_holders(path: &Path) -> Vec<String> {
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

pub(crate) fn format_errno(errno: Option<i32>) -> String {
    match errno {
        Some(libc::EBUSY) => "EBUSY(16)".to_string(),
        Some(libc::ENOENT) => "ENOENT(2)".to_string(),
        Some(libc::EINVAL) => "EINVAL(22)".to_string(),
        Some(libc::EPERM) => "EPERM(1)".to_string(),
        Some(value) => format!("errno({value})"),
        None => "unknown".to_string(),
    }
}
