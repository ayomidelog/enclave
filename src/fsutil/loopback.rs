//! Loop-device inventory.
//!
//! A quota-backed workspace mounts its `fs.img` through a loop device, so the
//! stop path and the doctor both need to answer "which loop device backs this
//! image" and "which loop devices exist at all". The kernel publishes both
//! answers under `/sys/block/loop*/loop/backing_file`, which costs a directory
//! read instead of a `losetup` fork and exec. `losetup` is kept as a fallback
//! for a host where `/sys` is not mounted.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::hostcmd::HostCommand;

/// Where the kernel exposes block-device attributes.
const SYS_BLOCK_DIR: &str = "/sys/block";

/// One attached loop device and the file it is backed by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LoopDevice {
    /// Absolute device path, for example `/dev/loop7`.
    pub device: String,
    /// The backing file as the kernel recorded it.
    pub backing: PathBuf,
}

/// Every attached loop device, from sysfs when it is readable and `losetup`
/// otherwise.
pub(crate) fn attached_loop_devices() -> Result<Vec<LoopDevice>> {
    if let Some(devices) = sysfs_loop_devices() {
        return Ok(devices);
    }
    let output = HostCommand::new("losetup")
        .arg("-a")
        .run_checked()
        .context("failed to list loop devices with losetup -a")?;
    Ok(parse_losetup_attached(&output.stdout_text()))
}

/// Loop devices whose backing file is `image`, sorted by device index.
pub(crate) fn loop_devices_for_image(image: &Path) -> Result<Vec<String>> {
    if let Some(devices) = sysfs_loop_devices() {
        return Ok(select_backing(&devices, image));
    }
    let output = HostCommand::new("losetup")
        .args(["-j"])
        .arg(image)
        .run_checked()
        .with_context(|| format!("failed to inspect loop devices for {}", image.display()))?;
    Ok(parse_losetup_for_image(&output.stdout_text()))
}

/// Every attached loop device under `sys_block`, or `None` when the directory
/// cannot be listed at all.
///
/// An unattached loop device has no `backing_file`, so a failed read there is
/// the ordinary "this device is free" case and is skipped.
pub(crate) fn sysfs_loop_devices_in(sys_block: &Path) -> Option<Vec<LoopDevice>> {
    let entries = fs::read_dir(sys_block).ok()?;
    let mut devices = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(index) = loop_index(name) else {
            continue;
        };
        let Ok(raw) = fs::read_to_string(entry.path().join("loop/backing_file")) else {
            continue;
        };
        let Some(backing) = normalize_backing(&raw) else {
            continue;
        };
        devices.push((
            index,
            LoopDevice {
                device: format!("/dev/{name}"),
                backing,
            },
        ));
    }
    devices.sort_by_key(|(index, _)| *index);
    Some(devices.into_iter().map(|(_, device)| device).collect())
}

fn sysfs_loop_devices() -> Option<Vec<LoopDevice>> {
    sysfs_loop_devices_in(Path::new(SYS_BLOCK_DIR))
}

/// The numeric index of a `loopN` block-device name.
fn loop_index(name: &str) -> Option<u32> {
    let suffix = name.strip_prefix("loop")?;
    if suffix.is_empty() || !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    suffix.parse().ok()
}

/// The backing path a kernel line names, with the deleted marker removed.
fn normalize_backing(raw: &str) -> Option<PathBuf> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let raw = raw.strip_suffix(" (deleted)").unwrap_or(raw);
    Some(PathBuf::from(raw))
}

/// Match attached devices against `image`.
///
/// The kernel records the path the attaching process used, which is absolute
/// here, but a symlinked or namespace-prefixed spelling would still name the
/// same file, so the canonical form is compared as well.
pub(crate) fn select_backing(devices: &[LoopDevice], image: &Path) -> Vec<String> {
    let canonical = image.canonicalize().ok();
    devices
        .iter()
        .filter(|device| {
            device.backing == image || canonical.as_deref() == Some(device.backing.as_path())
        })
        .map(|device| device.device.clone())
        .collect()
}

/// Every attached loop device, parsed from `losetup -a` output.
pub(crate) fn parse_losetup_attached(output: &str) -> Vec<LoopDevice> {
    output.lines().filter_map(parse_losetup_line).collect()
}

fn parse_losetup_line(line: &str) -> Option<LoopDevice> {
    let (device, rest) = line.split_once(':')?;
    let device = device.trim();
    loop_index(device.strip_prefix("/dev/")?)?;
    // The backing path is the parenthesised group that follows the device
    // numbers; a nested marker appears when the backing was removed.
    let start = rest.find('(')?;
    let end = rest.rfind(')')?;
    if end <= start {
        return None;
    }
    let backing = normalize_backing(&rest[start + 1..end])?;
    Some(LoopDevice {
        device: device.to_string(),
        backing,
    })
}

/// The devices `losetup -j <image>` reported for a single image.
pub(crate) fn parse_losetup_for_image(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| {
            let (device, _) = line.split_once(':')?;
            let device = device.trim();
            loop_index(device.strip_prefix("/dev/")?)?;
            Some(device.to_string())
        })
        .collect()
}
