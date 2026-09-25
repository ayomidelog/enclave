//! The loop device a quota-backed workspace image is attached to.
//!
//! An autoclear device is released by the kernel once its last holder closes, so
//! a device that is still attached but mounted nowhere is not a failure Enclave
//! can act on. The check that decides that is shared with the resource inventory
//! so the two cannot disagree about the same device.

use std::fs;
use std::path::Path;
use std::thread;
use std::time::Instant;

use anyhow::{bail, Result};

use crate::workspace::storage::{
    workspace_disk_image_path, workspace_uses_disk_image, LOOP_DETACH_POLL_INTERVAL,
};
use crate::workspace::types::WorkspaceMetadata;

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
