//! Asking the host what it currently holds.
//!
//! Every question here is answered from kernel state — `/proc`, `/sys`, the
//! mount table — rather than from anything Enclave recorded, so a test that
//! asserts a resource is gone is asking the host rather than the daemon.

use std::fs;
use std::path::Path;
use std::process::Command;

pub(crate) fn mounts_at_or_below(prefix: &Path) -> Vec<String> {
    let Ok(raw) = fs::read_to_string("/proc/self/mountinfo") else {
        return Vec::new();
    };
    let prefix = prefix.to_string_lossy();
    raw.lines()
        .filter_map(|line| line.split_whitespace().nth(4))
        .filter(|mount| {
            *mount == prefix.as_ref()
                || mount
                    .strip_prefix(prefix.as_ref())
                    .is_some_and(|rest| rest.starts_with("/"))
        })
        .map(str::to_string)
        .collect()
}

pub(crate) fn is_mountpoint(path: &str) -> bool {
    let Ok(raw) = fs::read_to_string("/proc/self/mountinfo") else {
        return false;
    };
    raw.lines()
        .any(|line| line.split_whitespace().nth(4) == Some(path))
}

pub(crate) fn enclave_rules_by_interface() -> Option<Vec<(String, usize)>> {
    let saved = Command::new("iptables-save").output().ok()?;
    if !saved.status.success() {
        return None;
    }
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for line in String::from_utf8_lossy(&saved.stdout).lines() {
        if !line.contains("enclave:") {
            continue;
        }
        for field in line.split_whitespace() {
            if field.starts_with("veth-") {
                *counts.entry(field.to_string()).or_default() += 1;
            }
        }
    }
    Some(counts.into_iter().collect())
}

pub(crate) fn workspace_cgroup_path(sandbox_id: &str, workspace_id: &str) -> std::path::PathBuf {
    Path::new("/sys/fs/cgroup")
        .join(format!("enclave-sb-{sandbox_id}"))
        .join(format!("enclave-ws-{sandbox_id}-{workspace_id}"))
}

pub(crate) fn process_starttime(pid: u32) -> Option<u64> {
    let raw = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    raw.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()
}

pub(crate) fn cgroup_processes(cgroup: &Path) -> Vec<(u32, u64)> {
    let Ok(raw) = fs::read_to_string(cgroup.join("cgroup.procs")) else {
        return Vec::new();
    };
    raw.lines()
        .filter_map(|line| line.trim().parse::<u32>().ok())
        .filter_map(|pid| process_starttime(pid).map(|starttime| (pid, starttime)))
        .collect()
}

pub(crate) fn persistent_helper_is_running(socket: &str) -> bool {
    let Ok(entries) = fs::read_dir("/proc") else {
        return false;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .filter(|name| name.bytes().all(|byte| byte.is_ascii_digit()))
        else {
            continue;
        };
        let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        // A zombie has already released its namespaces and cgroup membership.
        let state = stat
            .rsplit_once(") ")
            .and_then(|(_, rest)| rest.chars().next());
        if matches!(state, Some('Z') | Some('X')) {
            continue;
        }
        let Ok(cmdline) = fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        let cmdline = String::from_utf8_lossy(&cmdline);
        if cmdline.contains("workspace-session-persistent-helper") && cmdline.contains(socket) {
            return true;
        }
    }
    false
}

pub(crate) fn ext4_filesystem_size(image: &Path) -> std::io::Result<u64> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = fs::File::open(image)?;
    let mut superblock = [0u8; 1024];
    file.seek(SeekFrom::Start(1024))?;
    file.read_exact(&mut superblock)?;
    let magic = u16::from_le_bytes([superblock[0x38], superblock[0x39]]);
    if magic != 0xEF53 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{} is not an ext2/3/4 filesystem", image.display()),
        ));
    }
    let blocks_low = u64::from(u32::from_le_bytes(
        superblock[0x04..0x08].try_into().expect("4 bytes"),
    ));
    let log_block_size = u32::from_le_bytes(superblock[0x18..0x1c].try_into().expect("4 bytes"));
    let block_size = 1024u64 << log_block_size;
    let incompat = u32::from_le_bytes(superblock[0x60..0x64].try_into().expect("4 bytes"));
    let blocks = if incompat & 0x80 != 0 {
        blocks_low
            | (u64::from(u32::from_le_bytes(
                superblock[0x150..0x154].try_into().expect("4 bytes"),
            )) << 32)
    } else {
        blocks_low
    };
    Ok(blocks * block_size)
}

pub(crate) fn loop_devices_backing(image: &Path) -> Vec<String> {
    let mut devices = Vec::new();
    let Ok(entries) = fs::read_dir("/sys/class/block") else {
        return devices;
    };
    for entry in entries.flatten() {
        let Ok(backing) = fs::read_to_string(entry.path().join("loop/backing_file")) else {
            continue;
        };
        let backing = backing.trim();
        if !backing.is_empty() && image.ends_with(backing) {
            devices.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    devices
}

pub(crate) fn session_processes_for(sandbox_path: &Path) -> Vec<u32> {
    let marker = sandbox_path.to_string_lossy().into_owned();
    let Ok(entries) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .filter(|name| name.bytes().all(|byte| byte.is_ascii_digit()))
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(cmdline) = fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        let cmdline = String::from_utf8_lossy(&cmdline);
        if !cmdline.contains("workspace-session") || !cmdline.contains(&marker) {
            continue;
        }
        // A zombie holds no resources, so it is not an orphan.
        let state = fs::read_to_string(entry.path().join("stat"))
            .ok()
            .and_then(|stat| stat.rsplit_once(") ").map(|(_, rest)| rest.to_string()))
            .and_then(|rest| rest.chars().next());
        if matches!(state, Some('Z') | Some('X')) {
            continue;
        }
        found.push(pid);
    }
    found.sort_unstable();
    found
}
