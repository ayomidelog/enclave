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
use std::time::{Duration, Instant};

use enclave::operation::load;
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

/// Every mount point at or below a directory, as the host mount table lists them.
///
/// A workspace owns more than one mount: a quota-backed one has its disk image mounted
/// and its root overlay merged, and a stop has to release all of them. Asking for the
/// whole subtree is what makes a check for "no mount left" a statement about every mount
/// rather than about the one the test happened to know the name of.
pub(super) fn mounts_at_or_below(prefix: &Path) -> Vec<String> {
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

/// PIDs whose command line is a workspace session belonging to this sandbox directory.
///
/// A session helper carries the sandbox path in its arguments, so a process started for a
/// test's sandbox can be told apart from one serving another sandbox or a real one. A
/// zombie is skipped because it has already released its namespaces and holds nothing.
pub(super) fn session_processes_for(sandbox_path: &Path) -> Vec<u32> {
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

/// A daemon a test starts itself, so it can kill it or ask it to repair.
///
/// The CLI is used rather than the library because the daemon is the process under test:
/// a crash has to take the thing that owns the state with it, and an in-process call
/// cannot be killed without taking the test with it.
pub(super) struct TestDaemon {
    binary: PathBuf,
    state_dir: PathBuf,
    socket: PathBuf,
    pid_file: PathBuf,
    running: bool,
}

impl TestDaemon {
    pub(super) fn new(state_dir: &Path, socket_dir: &Path) -> Self {
        Self {
            binary: enclave_binary(),
            state_dir: state_dir.to_path_buf(),
            socket: socket_dir.join("manager.sock"),
            pid_file: socket_dir.join("manager.pid"),
            running: false,
        }
    }

    /// Run one CLI command against this daemon and return its output.
    pub(super) fn cli(&self, args: &[&str]) -> std::process::Output {
        Command::new(&self.binary)
            .arg("--socket")
            .arg(&self.socket)
            .args(args)
            .output()
            .expect("run the enclave CLI")
    }

    /// Run one CLI command and require it to succeed.
    pub(super) fn cli_ok(&self, args: &[&str]) {
        let output = self.cli(args);
        assert!(
            output.status.success(),
            "enclave {} failed: {}{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Run a CLI command in the background so a test can kill the daemon while it
    /// runs.
    pub(super) fn spawn(&self, args: &[&str]) -> std::process::Child {
        Command::new(&self.binary)
            .arg("--socket")
            .arg(&self.socket)
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn the enclave CLI")
    }

    /// Start the daemon and wait for it to answer.
    pub(super) fn start(&mut self) {
        let output = Command::new(&self.binary)
            .args(["--socket"])
            .arg(&self.socket)
            .args(["daemon", "start", "--state-dir"])
            .arg(&self.state_dir)
            .args(["--pid-file"])
            .arg(&self.pid_file)
            .args(["--wait-secs", "20"])
            .output()
            .expect("start the daemon");
        assert!(
            output.status.success(),
            "daemon start failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        self.running = true;
    }

    pub(super) fn pid(&self) -> u32 {
        fs::read_to_string(&self.pid_file)
            .expect("the daemon writes its pid file")
            .trim()
            .parse()
            .expect("the pid file holds a pid")
    }

    /// Kill the daemon the way a machine losing power would: no signal it can catch.
    pub(super) fn kill(&mut self) {
        assert!(self.running, "the daemon is not running");
        let pid = self.pid();
        let result = unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        assert_eq!(result, 0, "kill the daemon at pid {pid}");
        self.running = false;
        // The pid is reaped by its parent, and the socket is the only thing the next
        // start has to replace, so wait for the process to be gone rather than for a
        // fixed time.
        let deadline = Instant::now() + Duration::from_secs(10);
        while Path::new(&format!("/proc/{pid}")).exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !Path::new(&format!("/proc/{pid}")).exists(),
            "daemon pid {pid} survived SIGKILL"
        );
    }

    /// The phase the newest journal record of one kind is in, when there is one.
    pub(super) fn phase_of(&self, kind: &str) -> Option<String> {
        let root = self.state_dir.join("operations");
        let entries = fs::read_dir(root).ok()?;
        let mut newest: Option<(String, String)> = None;
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(id) = path.file_stem().and_then(|name| name.to_str()) else {
                continue;
            };
            let Ok(record) = load(&self.state_dir, id) else {
                continue;
            };
            if record.kind != kind {
                continue;
            }
            if newest
                .as_ref()
                .is_none_or(|(current, _)| record.updated_at >= *current)
            {
                newest = Some((record.updated_at.clone(), record.phase.clone()));
            }
        }
        newest.map(|(_, phase)| phase)
    }

    /// Block until the operation of one kind reaches a phase, or give up.
    ///
    /// Returns whether the phase was reached. A caller that kills on a false return is
    /// testing nothing, so every caller asserts on it.
    pub(super) fn wait_for_phase(&self, kind: &str, phase: &str, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.phase_of(kind).as_deref() == Some(phase) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        false
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        if self.running {
            let _ = self.cli(&["daemon", "stop"]);
        }
    }
}

pub(super) fn enclave_binary() -> PathBuf {
    if let Ok(path) = std::env::var("CARGO_BIN_EXE_enclave") {
        return PathBuf::from(path);
    }
    let exe = std::env::current_exe().expect("resolve the test executable");
    exe.parent()
        .and_then(Path::parent)
        .expect("resolve the debug directory")
        .join("enclave")
}

/// The sandbox directory whose name carries the given name.
pub(super) fn sandbox_dir(state_dir: &Path, name: &str) -> PathBuf {
    let root = state_dir.join("sandboxes");
    let mut matches = fs::read_dir(&root)
        .expect("read the sandboxes directory")
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|file| file.to_str())
                .is_some_and(|file| file.starts_with(&format!("{name}-")))
        })
        .collect::<Vec<_>>();
    matches.sort();
    matches
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("no sandbox directory for {name} under {}", root.display()))
}

/// The workspace directory whose name carries the given name.
pub(super) fn workspace_dir(sandbox: &Path, name: &str) -> PathBuf {
    let root = sandbox.join("workspaces");
    let mut matches = fs::read_dir(&root)
        .expect("read the workspaces directory")
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|file| file.to_str())
                .is_some_and(|file| file.starts_with(&format!("{name}-")))
        })
        .collect::<Vec<_>>();
    matches.sort();
    matches
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("no workspace directory for {name} under {}", root.display()))
}
