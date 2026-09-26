//! The fixture a crash test builds and the assertion it ends with.

use std::fs;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::time::{Duration, Instant};

use enclave::operation::{load, OperationStatus};

use super::super::support::{
    cgroup_processes, loop_devices_backing, prepare_cached_rootfs, process_starttime, sandbox_dir,
    session_processes_for, state_dir, workspace_cgroup_path, workspace_dir, TestDaemon,
};

/// Every mount under the workspaces of a state directory.
///
/// The sandbox rootfs overlay is excluded on purpose: it is mounted for a sandbox's
/// whole life, running or not, so it is not a leak. A workspace's own storage is, because
/// it is mounted only while the workspace runs.
fn workspace_mounts(state_dir: &Path) -> Vec<String> {
    let Ok(raw) = fs::read_to_string("/proc/self/mountinfo") else {
        return Vec::new();
    };
    let workspaces = state_dir.join("sandboxes");
    raw.lines()
        .filter_map(|line| line.split_whitespace().nth(4))
        .filter(|mount| mount.starts_with(&workspaces.to_string_lossy().to_string()))
        .filter(|mount| mount.contains("/workspaces/"))
        .map(str::to_string)
        .collect()
}

/// Every Enclave interface this state directory's sandbox owns.
///
/// An interface is named from the workspace id and the address it was given, so the
/// names of the sandbox's own workspaces are what to look for rather than every
/// interface on the host, which may belong to another state directory.
fn sandbox_veths(state_dir: &Path, sandbox_id: &str) -> Vec<String> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir("/sys/class/net") else {
        return found;
    };
    let workspace_ids = workspace_ids_of(state_dir, sandbox_id);
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(rest) = name.strip_prefix("veth-") else {
            continue;
        };
        let Some((_octet, hash)) = rest.split_once('-') else {
            continue;
        };
        if workspace_ids.iter().any(|id| veth_hash(id) == hash) {
            found.push(name);
        }
    }
    found
}

/// The workspace ids a sandbox's directories carry.
fn workspace_ids_of(state_dir: &Path, sandbox_id: &str) -> Vec<String> {
    let root = state_dir
        .join("sandboxes")
        .join(sandbox_id)
        .join("workspaces");
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect()
}

/// The same hash the interface name uses, so a test can find a workspace's interface
/// without asking the daemon.
fn veth_hash(workspace_id: &str) -> String {
    let hash = workspace_id.bytes().fold(0x811c9dc5u32, |hash, byte| {
        hash.wrapping_mul(0x01000193) ^ u32::from(byte)
    }) & 0x00ff_ffff;
    format!("{hash:06x}")
}

/// Every journal record in the state directory, with its status.
fn journal_records(state_dir: &Path) -> Vec<(String, OperationStatus, String)> {
    let root = state_dir.join("operations");
    let Ok(entries) = fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut records = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(id) = path.file_stem().and_then(|name| name.to_str()) else {
            continue;
        };
        if let Ok(record) = load(state_dir, id) {
            records.push((record.id.clone(), record.status, record.kind.clone()));
        }
    }
    records
}

/// What a crash test starts from: a daemon, a sandbox, and one workspace.
pub(super) struct CrashFixture {
    // The test modules that share this fixture drive the daemon and read the
    // names directly, so the fields are visible across the suite rather than
    // behind an accessor for each one.
    pub(super) state: PathBuf,
    pub(super) socket_dir: PathBuf,
    pub(super) sandbox_name: String,
    pub(super) workspace_name: String,
    pub(super) daemon: TestDaemon,
}

impl CrashFixture {
    /// Build the fixture and leave the daemon running with the workspace stopped.
    pub(super) fn new(label: &str) -> Self {
        Self::build(label, None)
    }

    /// Build the fixture with a quota-backed workspace.
    ///
    /// The quota tier is a different kind of storage: the workspace owns an ext4
    /// image on a loop device rather than a directory, so a crash while it is being
    /// torn down leaves kernel state a directory-backed workspace never has.
    pub(super) fn quota(label: &str, disk_mb: u32) -> Self {
        Self::build(label, Some(disk_mb))
    }

    fn build(label: &str, disk_mb: Option<u32>) -> Self {
        let state = state_dir(&format!("enclave-int-crash-{label}"));
        prepare_cached_rootfs(&state, "bookworm");
        let socket_dir = std::env::temp_dir().join(format!(
            "enclave-int-crash-{label}-socket-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&socket_dir);
        fs::create_dir_all(&socket_dir).expect("create the socket directory");

        let sandbox_name = "crashbox".to_string();
        let workspace_name = "dev".to_string();
        let mut daemon = TestDaemon::new(&state, &socket_dir);
        daemon.start();
        daemon.cli_ok(&[
            "create",
            &sandbox_name,
            "--suite",
            "bookworm",
            "--bootstrap-method",
            "cached_rootfs",
        ]);
        let mut create = vec![
            "workspace",
            "create",
            sandbox_name.as_str(),
            workspace_name.as_str(),
        ];
        let disk_arg;
        if let Some(disk_mb) = disk_mb {
            disk_arg = disk_mb.to_string();
            create.extend(["--disk-mb", disk_arg.as_str()]);
        }
        daemon.cli_ok(&create);
        // `workspace create` starts what it creates, so the workspace is running here.
        // Every crash test starts from the same place, a stopped workspace, which means
        // stopping it: a start of a workspace that is already running is answered from
        // the live runtime and never enters the launch the launch test means to kill.
        daemon.cli_ok(&["workspace", "stop", &sandbox_name, &workspace_name]);

        Self {
            state,
            socket_dir,
            sandbox_name,
            workspace_name,
            daemon,
        }
    }

    pub(super) fn sandbox_dir(&self) -> PathBuf {
        sandbox_dir(&self.state, &self.sandbox_name)
    }

    pub(super) fn workspace_dir(&self) -> PathBuf {
        workspace_dir(&self.sandbox_dir(), &self.workspace_name)
    }

    pub(super) fn sandbox_id(&self) -> String {
        self.sandbox_dir()
            .file_name()
            .expect("the sandbox directory has a name")
            .to_string_lossy()
            .into_owned()
    }

    pub(super) fn workspace_id(&self) -> String {
        self.workspace_dir()
            .file_name()
            .expect("the workspace directory has a name")
            .to_string_lossy()
            .into_owned()
    }

    /// Block until the workspace's session has recorded its own pid, or give up.
    ///
    /// The launch phase is entered before the session exists, so a kill on the
    /// phase alone can land before there is a runtime to leave behind. Waiting for
    /// the pid file the session writes from inside its namespace puts the kill
    /// inside the window this test is about: a runtime that is running and a
    /// record that does not name it yet.
    pub(super) fn wait_for_session(&self, timeout: Duration) -> bool {
        let pid_file = self.workspace_dir().join("runtime").join("session.pid");
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if pid_file.exists() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        false
    }

    /// Block until the workspace's interface exists on the host, or give up.
    ///
    /// The network phase runs inside the launch, so no journal phase names it. The
    /// interface itself does: it is created before the address is attached and the
    /// anti-spoofing rules are installed, and the launch cannot commit until all of
    /// that is done. Waiting for it is what puts the kill inside the window where
    /// the host holds network state the record does not describe yet.
    pub(super) fn wait_for_veth(&self, timeout: Duration) -> bool {
        let sandbox_id = self.sandbox_id();
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if !sandbox_veths(&self.state, &sandbox_id).is_empty() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        false
    }

    /// Run one CLI command against the fixture's daemon.
    pub(super) fn cli_ok(&self, args: &[&str]) {
        self.daemon.cli_ok(args);
    }

    /// Run a command in the background so the test can kill the daemon while it runs.
    pub(super) fn spawn(&self, args: &[&str]) -> Child {
        self.daemon.spawn(args)
    }

    /// Take the registry lock so the daemon cannot commit a transition.
    pub(super) fn hold_registry_lock(&self) -> RegistryLockGuard {
        RegistryLockGuard::acquire(&self.state.join("registry.lock"))
    }

    /// The lifecycle fields the registry records for the fixture's workspace.
    ///
    /// Read from the file rather than through the CLI or the registry library on
    /// purpose: a crash test holds the registry lock to keep a window open, and
    /// either of those takes the lock and would block on it.
    pub(super) fn registry_record(&self) -> RecordedWorkspace {
        let path = self.state.join("registry.json");
        let raw = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read the registry {}: {error}", path.display()));
        let registry: serde_json::Value =
            serde_json::from_str(&raw).expect("the registry is valid json");
        let sandbox_id = self.sandbox_id();
        let workspace_id = self.workspace_id();
        let record = &registry["sandboxes"][&sandbox_id]["workspaces"][&workspace_id];
        assert!(
            !record.is_null(),
            "the registry does not record workspace '{workspace_id}'"
        );
        RecordedWorkspace::from_json(record)
    }

    /// The status the workspace's own `workspace.json` records.
    ///
    /// This is the copy repair adopts, and it is written before the registry commit,
    /// so a test that wants to land between the two has to watch this file rather
    /// than the registry.
    pub(super) fn disk_workspace_status(&self) -> String {
        let path = self.workspace_dir().join("workspace.json");
        let raw = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        let record: serde_json::Value =
            serde_json::from_str(&raw).expect("the workspace metadata is valid json");
        record["status"]
            .as_str()
            .unwrap_or_else(|| panic!("{} records no status", path.display()))
            .to_string()
    }

    /// Block until the workspace's own record reaches `status`, or give up.
    pub(super) fn wait_for_disk_status(&self, status: &str, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.disk_workspace_status() == status {
                return true;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        false
    }
}

impl Drop for CrashFixture {
    fn drop(&mut self) {
        // The daemon may be dead, in which case the stop is a no-op. Whatever is left is
        // removed directly, because a test that failed has no daemon to ask.
        let _ = self.daemon.cli(&["destroy", "--force", &self.sandbox_name]);
        let _ = self.daemon.cli(&["daemon", "stop"]);
        let _ = fs::remove_dir_all(&self.state);
        let _ = fs::remove_dir_all(&self.socket_dir);
    }
}

/// The registry lock, held for as long as this guard lives.
///
/// The commit a start performs is the only step of a launch that takes the
/// registry lock, so a test that holds the lock from outside decides when the
/// commit can happen rather than racing it. That is what makes the window
/// between a running runtime and the record that names it a window a test can
/// act inside: it stays open until the guard is dropped.
///
/// The lock is `flock(2)` on the same file the daemon uses, so holding it blocks
/// the daemon's own attempt rather than being advisory to this process alone.
pub(super) struct RegistryLockGuard {
    file: fs::File,
}

impl RegistryLockGuard {
    /// Take the registry lock, waiting briefly if the daemon is between writes.
    fn acquire(path: &Path) -> Self {
        let file = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)
            .unwrap_or_else(|error| panic!("open the registry lock {}: {error}", path.display()));
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if rc == 0 {
                return Self { file };
            }
            let error = std::io::Error::last_os_error();
            assert!(
                Instant::now() < deadline,
                "could not take the registry lock {}: {error}",
                path.display()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for RegistryLockGuard {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

/// The lifecycle fields of one workspace record, read out of the registry file.
///
/// Only the fields that describe the runtime are kept: those are the ones recovery
/// has to make agree with the host, and the rest of the record is metadata that no
/// crash can put out of step with it.
#[derive(Debug)]
pub(super) struct RecordedWorkspace {
    pub(super) status: String,
    pub(super) runtime_pid: Option<u32>,
    pub(super) runtime_starttime_ticks: Option<u64>,
    pub(super) assigned_ip: Option<String>,
}

impl RecordedWorkspace {
    fn from_json(record: &serde_json::Value) -> Self {
        Self {
            status: record["status"]
                .as_str()
                .expect("the workspace record has a status")
                .to_string(),
            runtime_pid: record["runtime_pid"]
                .as_u64()
                .and_then(|pid| u32::try_from(pid).ok()),
            runtime_starttime_ticks: record["runtime_starttime_ticks"].as_u64(),
            assigned_ip: record["assigned_ip"].as_str().map(str::to_string),
        }
    }
}

/// Wait for a spawned CLI command to finish, so a test does not leave a process behind.
pub(super) fn reap(mut child: Child) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if child.try_wait().expect("poll the CLI").is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// What a leftover process is, and what the daemon said about it.
///
/// A leftover process is the one finding that cannot be investigated after the fact:
/// the fixture's guard stops the daemon and removes its state directory and its log, so
/// a failure that reports only a pid leaves nothing behind to explain it. Both are
/// captured here, while they still exist.
fn orphan_diagnostics(fixture: &CrashFixture, orphans: &[u32]) -> String {
    let mut out = String::new();
    for pid in orphans {
        let cmdline = fs::read(format!("/proc/{pid}/cmdline"))
            .map(|raw| String::from_utf8_lossy(&raw).replace('\0', " "))
            .unwrap_or_else(|error| format!("<unreadable: {error}>"));
        let cgroup = fs::read_to_string(format!("/proc/{pid}/cgroup"))
            .map(|raw| raw.trim().replace('\n', "; "))
            .unwrap_or_else(|error| format!("<unreadable: {error}>"));
        out.push_str(&format!(
            "\n  orphan {pid} cmdline: {cmdline}\n  orphan {pid} cgroup: {cgroup}"
        ));
    }
    out.push_str("\n  daemon log tail:\n");
    out.push_str(&fixture.daemon.log_tail(60));
    for (label, path) in [
        ("registry", fixture.state.join("registry.json")),
        (
            "workspace.json",
            fixture.workspace_dir().join("workspace.json"),
        ),
    ] {
        let record = fs::read_to_string(&path)
            .map(|raw| {
                serde_json::from_str::<serde_json::Value>(&raw)
                    .map(|value| value.to_string())
                    .unwrap_or(raw)
            })
            .unwrap_or_else(|error| format!("<unreadable: {error}>"));
        out.push_str(&format!("\n  {label}: {record}"));
    }
    out
}

/// What every crash test asserts once the daemon has restarted.
///
/// The workspace has to be in one of the two settled states, and whichever it is in has
/// to match the host: a running workspace has a live runtime in its cgroup, and a stopped
/// one holds no runtime, no cgroup, no storage mount, and no interface. The journal has
/// to be terminal, because the restart is the one moment that closes what a dead daemon
/// left open.
pub(super) fn assert_recovered(fixture: &CrashFixture) {
    let sandbox_id = fixture.sandbox_id();
    let workspace_id = fixture.workspace_id();
    let sandbox_path = fixture.sandbox_dir();

    // The journal has to be terminal in either settled state: the restart is the one
    // moment that closes what a dead daemon left open, and a record left `planned` or
    // `running` makes doctor report the same interrupted operation forever.
    let records = journal_records(&fixture.state);
    let open = records
        .iter()
        .filter(|(_, status, _)| {
            matches!(status, OperationStatus::Planned | OperationStatus::Running)
        })
        .map(|(id, _, kind)| format!("{id} ({kind})"))
        .collect::<Vec<_>>();
    assert!(
        open.is_empty(),
        "recovery left open journal record(s): {open:?}"
    );

    // The record has to describe the host, and which settled state it settled in is
    // what decides what the host is allowed to hold. A rollback has to have released
    // everything the interrupted operation created; a start whose own record was
    // written before the commit is a workspace the next daemon completes, and its
    // runtime, cgroup, mounts, and interface are then described by that record rather
    // than leaked by it. Asserting on emptiness alone would call the second outcome a
    // leak and the first one a pass, which is the mistake this branch exists to avoid.
    let record = fixture.registry_record();
    let live = session_processes_for(&sandbox_path);
    let cgroup = workspace_cgroup_path(&sandbox_id, &workspace_id);
    match record.status.as_str() {
        "running" => {
            let pid = record
                .runtime_pid
                .unwrap_or_else(|| panic!("a running workspace records no runtime: {record:?}"));
            let beside = live
                .iter()
                .copied()
                .filter(|live_pid| *live_pid != pid)
                .collect::<Vec<_>>();
            assert!(
                beside.is_empty(),
                "recovery left session process(es) {beside:?} beside the recorded runtime {pid}{}",
                orphan_diagnostics(fixture, &beside)
            );
            assert_eq!(
                process_starttime(pid),
                record.runtime_starttime_ticks,
                "the record names a runtime that is not running with its start time"
            );
            assert!(
                cgroup.exists(),
                "a running workspace has no cgroup at {}",
                cgroup.display()
            );
            assert!(
                cgroup_processes(&cgroup)
                    .iter()
                    .any(|(held, _)| *held == pid),
                "the workspace runtime is not in its cgroup {}",
                cgroup.display()
            );
            assert!(
                record.assigned_ip.is_some(),
                "a running workspace holds no address"
            );
        }
        "stopped" => {
            assert!(
                live.is_empty(),
                "recovery left {} session process(es) behind: {live:?}{}",
                live.len(),
                orphan_diagnostics(fixture, &live)
            );
            assert_eq!(
                record.runtime_pid, None,
                "a stopped workspace still records a runtime"
            );
            assert_eq!(
                record.assigned_ip, None,
                "a stopped workspace still holds an address"
            );

            let mounts = workspace_mounts(&fixture.state);
            assert!(
                mounts.is_empty(),
                "recovery left workspace mount(s) behind: {mounts:?}"
            );

            let veths = sandbox_veths(&fixture.state, &sandbox_id);
            assert!(
                veths.is_empty(),
                "recovery left interface(s) behind: {veths:?}"
            );

            assert!(
                !cgroup.exists(),
                "recovery left the workspace cgroup {} behind",
                cgroup.display()
            );

            // A quota-backed workspace owns an ext4 image on a loop device. That is
            // kernel state a directory-backed workspace never has, and it is released
            // by the same teardown, so a rollback that stopped early leaves the image
            // attached with nothing left in the registry to find it by.
            let image = Path::new(&fixture.workspace_dir()).join("fs.img");
            if image.exists() {
                let loops = loop_devices_backing(&image);
                assert!(
                    loops.is_empty(),
                    "recovery left the workspace image attached to {loops:?}"
                );
            }
        }
        other => panic!("recovery left the workspace {other}: {record:?}"),
    }
}
