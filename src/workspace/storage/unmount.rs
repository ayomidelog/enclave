use super::*;

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
pub(crate) fn verify_no_mounts_below(root: &Path) -> Result<()> {
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
    let deadline = Instant::now() + crate::deadlines::loop_detach().get();
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

pub(crate) fn loop_devices_for_image(image: &Path) -> Result<Vec<String>> {
    let output = HostCommand::new("losetup")
        .args(["-j"])
        .arg(image)
        .run_checked()
        .with_context(|| format!("failed to inspect loop devices for {}", image.display()))?;
    Ok(parse_loop_devices(&output.stdout_text()))
}

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

pub(crate) fn parse_loop_devices(output: &str) -> Vec<String> {
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
