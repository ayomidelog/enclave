//! Shared fixtures and host probes for the privileged integration tests.
//!
//! Every test builds its own state directory and its own cached rootfs, so these
//! are about constructing a sandbox the daemon can start rather than about
//! sharing state between tests. The host probes are here for the same reason:
//! several test files ask the host the same question about a cgroup, a mount, or
//! a process, and answering it in one place keeps the answers consistent.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use enclave::sandbox::destroy_sandbox;

pub(super) fn root_only() -> bool {
    unsafe { libc::geteuid() == 0 }
}

/// The smallest rootfs the daemon can start a workspace on.
///
/// The daemon bootstraps from a cached rootfs, so a test only has to provide a
/// tree that looks like one. It is a busybox shell plus the applets the tests run
/// through it, and an `/opt` for the tests that fill a quota-backed workspace's
/// disk by writing into it. Two files used to keep their own near-identical copy
/// of this, which is how one of them ended up without the `/opt` the other
/// needed; the union is the fixture.
pub(super) fn prepare_cached_rootfs(state_dir: &Path, suite: &str) {
    let cache = state_dir.join("sandboxes").join("rootfs-cache").join(suite);
    for directory in ["bin", "etc", "opt", "usr/bin"] {
        fs::create_dir_all(cache.join(directory)).expect("create rootfs directory");
    }
    fs::copy("/usr/bin/busybox", cache.join("bin").join("busybox")).expect("copy busybox");
    // Relative links, so the fixture does not depend on the workspace root being
    // the process root when an applet is resolved.
    for (link, target) in [
        ("bin/sh", "busybox"),
        ("bin/cat", "busybox"),
        ("bin/dd", "busybox"),
        ("usr/bin/env", "../../bin/busybox"),
    ] {
        std::os::unix::fs::symlink(target, cache.join(link)).expect("link busybox applet");
    }
}

pub(super) fn state_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create state dir");
    dir
}

/// Whether the mount table currently lists `path` as a mount point.
pub(super) fn is_mountpoint(path: &str) -> bool {
    let Ok(raw) = fs::read_to_string("/proc/self/mountinfo") else {
        return false;
    };
    raw.lines()
        .any(|line| line.split_whitespace().nth(4) == Some(path))
}

/// Every interface named by an Enclave-owned firewall rule, with its rule count.
pub(super) fn enclave_rules_by_interface() -> Option<Vec<(String, usize)>> {
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

/// Absolute path of the cgroup that holds a workspace's processes.
///
/// The naming is a contract the daemon and doctor both rely on, so the test
/// builds it from the ids rather than reaching into the crate.
pub(super) fn workspace_cgroup_path(sandbox_id: &str, workspace_id: &str) -> std::path::PathBuf {
    Path::new("/sys/fs/cgroup")
        .join(format!("enclave-sb-{sandbox_id}"))
        .join(format!("enclave-ws-{sandbox_id}-{workspace_id}"))
}

/// Field 22 of /proc/<pid>/stat: the process start time in clock ticks.
///
/// The command name can contain spaces and parentheses, so the fields are counted
/// from the last closing parenthesis rather than from the start of the line.
pub(super) fn process_starttime(pid: u32) -> Option<u64> {
    let raw = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    raw.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()
}

/// The host PIDs in a cgroup, each with the start time that identifies it across
/// PID reuse.
pub(super) fn cgroup_processes(cgroup: &Path) -> Vec<(u32, u64)> {
    let Ok(raw) = fs::read_to_string(cgroup.join("cgroup.procs")) else {
        return Vec::new();
    };
    raw.lines()
        .filter_map(|line| line.trim().parse::<u32>().ok())
        .filter_map(|pid| process_starttime(pid).map(|starttime| (pid, starttime)))
        .collect()
}

/// Removes the sandbox a test created and its state directory when the test ends.
pub(super) struct SandboxCleanup {
    state_dir: PathBuf,
    sandbox_id: Option<String>,
}

impl SandboxCleanup {
    pub(super) fn new(state_dir: PathBuf) -> Self {
        Self {
            state_dir,
            sandbox_id: None,
        }
    }

    pub(super) fn record(&mut self, sandbox_id: &str) {
        self.sandbox_id = Some(sandbox_id.to_string());
    }
}

impl Drop for SandboxCleanup {
    fn drop(&mut self) {
        if let Some(sandbox_id) = self.sandbox_id.as_deref() {
            let _ = destroy_sandbox(&self.state_dir, sandbox_id);
        }
        let _ = fs::remove_dir_all(&self.state_dir);
    }
}

/// Whether a persistent workspace session helper is still running for `socket`.
pub(super) fn persistent_helper_is_running(socket: &str) -> bool {
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

/// Size of the ext2/3/4 filesystem recorded in a disk image's superblock.
///
/// Resizing an image can grow the file while leaving the filesystem inside it
/// at the previous size, so tests assert the filesystem itself reached the
/// requested allocation instead of trusting the image file size.
pub(super) fn ext4_filesystem_size(image: &Path) -> std::io::Result<u64> {
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

/// Loop devices on the host whose backing file is the given image.
///
/// The kernel exposes the backing file for each loop device in sysfs, so this
/// reads the kernel's own record rather than parsing command output.
pub(super) fn loop_devices_backing(image: &Path) -> Vec<String> {
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
