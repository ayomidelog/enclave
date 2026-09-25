//! What a daemon killed in the middle of a lifecycle operation leaves behind.
//!
//! Every other test in this directory drives the lifecycle in-process, which is why none
//! of them can test a crash: there is no second process to kill. These run the real
//! daemon in its own state directory and kill it with SIGKILL, which is the one
//! interruption the daemon cannot catch and answer for itself. What is asserted is not
//! that the interrupted operation succeeded, because it cannot: it is that the state the
//! next daemon start inherits is one it can recover from without an operator.
//!
//! The kill point is chosen from the operation's own journal rather than from a delay.
//! A journal record names the phase it is in before that phase does its work, so waiting
//! for a phase and then killing lands inside the phase every time, on a fast host and a
//! slow one alike. A delay would land wherever the host happened to be, which is the same
//! thing as not choosing.
//!
//! What recovery owes is a settled state that agrees with the host. The workspace is
//! either running with a live runtime or stopped with nothing of its own left on the
//! host, and either way nothing is left running that no record describes.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use enclave::operation::{load, OperationStatus};

use super::support::{
    prepare_cached_rootfs, root_only, session_processes_for, state_dir, workspace_cgroup_path,
};

/// A daemon this test starts itself, so it can kill it.
///
/// The CLI is used rather than the library because the daemon is the process under test:
/// a crash has to take the thing that owns the state with it, and an in-process call
/// cannot be killed without taking the test with it.
struct TestDaemon {
    binary: PathBuf,
    state_dir: PathBuf,
    socket: PathBuf,
    pid_file: PathBuf,
    running: bool,
}

impl TestDaemon {
    fn new(state_dir: &Path, socket_dir: &Path) -> Self {
        Self {
            binary: enclave_binary(),
            state_dir: state_dir.to_path_buf(),
            socket: socket_dir.join("manager.sock"),
            pid_file: socket_dir.join("manager.pid"),
            running: false,
        }
    }

    /// Run one CLI command against this daemon and return its output.
    fn cli(&self, args: &[&str]) -> std::process::Output {
        Command::new(&self.binary)
            .arg("--socket")
            .arg(&self.socket)
            .args(args)
            .output()
            .expect("run the enclave CLI")
    }

    /// Run one CLI command and require it to succeed.
    fn cli_ok(&self, args: &[&str]) {
        let output = self.cli(args);
        assert!(
            output.status.success(),
            "enclave {} failed: {}{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Start the daemon and wait for it to answer.
    fn start(&mut self) {
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

    fn pid(&self) -> u32 {
        fs::read_to_string(&self.pid_file)
            .expect("the daemon writes its pid file")
            .trim()
            .parse()
            .expect("the pid file holds a pid")
    }

    /// Kill the daemon the way a machine losing power would: no signal it can catch.
    fn kill(&mut self) {
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
    fn phase_of(&self, kind: &str) -> Option<String> {
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
    fn wait_for_phase(&self, kind: &str, phase: &str, timeout: Duration) -> bool {
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

fn enclave_binary() -> PathBuf {
    if let Ok(path) = std::env::var("CARGO_BIN_EXE_enclave") {
        return PathBuf::from(path);
    }
    let exe = std::env::current_exe().expect("resolve the test executable");
    exe.parent()
        .and_then(Path::parent)
        .expect("resolve the debug directory")
        .join("enclave")
}

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

/// The sandbox directory whose name carries the given name.
fn sandbox_dir(state_dir: &Path, name: &str) -> PathBuf {
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
fn workspace_dir(sandbox: &Path, name: &str) -> PathBuf {
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

/// What a crash test starts from: a daemon, a sandbox, and one workspace.
struct CrashFixture {
    state: PathBuf,
    socket_dir: PathBuf,
    sandbox_name: String,
    workspace_name: String,
    daemon: TestDaemon,
}

impl CrashFixture {
    /// Build the fixture and leave the daemon running with the workspace stopped.
    fn new(label: &str) -> Self {
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
        daemon.cli_ok(&["workspace", "create", &sandbox_name, &workspace_name]);
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

    fn sandbox_dir(&self) -> PathBuf {
        sandbox_dir(&self.state, &self.sandbox_name)
    }

    fn workspace_dir(&self) -> PathBuf {
        workspace_dir(&self.sandbox_dir(), &self.workspace_name)
    }

    fn sandbox_id(&self) -> String {
        self.sandbox_dir()
            .file_name()
            .expect("the sandbox directory has a name")
            .to_string_lossy()
            .into_owned()
    }

    fn workspace_id(&self) -> String {
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
    fn wait_for_session(&self, timeout: Duration) -> bool {
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

    /// Run one CLI command against the fixture's daemon.
    fn cli_ok(&self, args: &[&str]) {
        self.daemon.cli_ok(args);
    }

    /// Run a command in the background so the test can kill the daemon while it runs.
    fn spawn(&self, args: &[&str]) -> Child {
        Command::new(&self.daemon.binary)
            .arg("--socket")
            .arg(&self.daemon.socket)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn the enclave CLI")
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

/// Wait for a spawned CLI command to finish, so a test does not leave a process behind.
fn reap(mut child: Child) {
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

/// What every crash test asserts once the daemon has restarted.
///
/// The workspace has to be in one of the two settled states, and whichever it is in has
/// to match the host: a running workspace has a live runtime in its cgroup, and a stopped
/// one holds no runtime, no cgroup, no storage mount, and no interface. The journal has
/// to be terminal, because the restart is the one moment that closes what a dead daemon
/// left open.
fn assert_recovered(fixture: &CrashFixture) {
    let sandbox_id = fixture.sandbox_id();
    let workspace_id = fixture.workspace_id();
    let sandbox_path = fixture.sandbox_dir();

    let orphans = session_processes_for(&sandbox_path);
    assert!(
        orphans.is_empty(),
        "recovery left {} session process(es) behind: {orphans:?}",
        orphans.len()
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

    let cgroup = workspace_cgroup_path(&sandbox_id, &workspace_id);
    assert!(
        !cgroup.exists(),
        "recovery left the workspace cgroup {} behind",
        cgroup.display()
    );

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

    // The record has to describe the host, which for a crashed operation means it was
    // rolled back rather than left transitional.
    let status = fixture.daemon.cli(&[
        "workspace",
        "status",
        &fixture.sandbox_name,
        &fixture.workspace_name,
    ]);
    let stdout = String::from_utf8_lossy(&status.stdout).into_owned();
    assert!(
        stdout.contains("status: stopped") || stdout.contains("status: running"),
        "the workspace is not in a settled state: {stdout}{}",
        String::from_utf8_lossy(&status.stderr)
    );
}

/// A daemon killed while a workspace is starting must leave a state the next start
/// recovers from.
///
/// The launch is the phase that has already created the runtime, its cgroup, the
/// interface, and the storage mounts by the time it is reached, so a crash there is the
/// case with the most to release. Recovery rolls the transition back rather than resuming
/// it, because the identity a half-finished launch recorded may not be the process that
/// is actually running.
#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn a_daemon_killed_during_a_workspace_launch_recovers() {
    if !root_only() {
        return;
    }

    let mut fixture = CrashFixture::new("launch");
    let child = fixture.spawn(&[
        "workspace",
        "start",
        &fixture.sandbox_name,
        &fixture.workspace_name,
    ]);
    assert!(
        fixture
            .daemon
            .wait_for_phase("workspace.start", "launch_runtime", Duration::from_secs(20)),
        "the start never reached its launch phase, so killing here would test nothing"
    );
    assert!(
        fixture.wait_for_session(Duration::from_secs(20)),
        "the launch never produced a session, so there is no runtime to leave behind"
    );
    fixture.daemon.kill();
    reap(child);
    fixture.daemon.start();

    assert_recovered(&fixture);
}

/// A daemon killed between a workspace's runtime launching and its metadata committing
/// must leave a state the next start recovers from.
///
/// This is the narrowest window in the lifecycle and the one with the most to get wrong:
/// the runtime exists and is running, and the registry still says the workspace is
/// starting. A recovery that trusted the record would leave a runtime nothing describes;
/// one that trusted the process would adopt a runtime whose identity was never committed.
/// What the daemon does instead is roll back, which is what this asserts.
#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn a_daemon_killed_before_a_workspace_start_commits_recovers() {
    if !root_only() {
        return;
    }

    let mut fixture = CrashFixture::new("commit");
    let child = fixture.spawn(&[
        "workspace",
        "start",
        &fixture.sandbox_name,
        &fixture.workspace_name,
    ]);
    assert!(
        fixture.daemon.wait_for_phase(
            "workspace.start",
            "commit_runtime_metadata",
            Duration::from_secs(20)
        ),
        "the start never reached its commit phase, so killing here would test nothing"
    );
    fixture.daemon.kill();
    reap(child);
    fixture.daemon.start();

    assert_recovered(&fixture);
}

/// A daemon killed while a workspace is stopping must leave a state the next start
/// recovers from.
///
/// The stop phase is reached after the runtime has been signalled and before its cgroup,
/// its interface, its firewall rules, and its storage mounts have been released, so a
/// crash there leaves host state that the record still describes. Recovery reads that
/// record and finishes the teardown.
#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn a_daemon_killed_during_a_workspace_stop_recovers() {
    if !root_only() {
        return;
    }

    let mut fixture = CrashFixture::new("stop");
    fixture.cli_ok(&[
        "workspace",
        "start",
        &fixture.sandbox_name,
        &fixture.workspace_name,
    ]);
    let child = fixture.spawn(&[
        "workspace",
        "stop",
        &fixture.sandbox_name,
        &fixture.workspace_name,
    ]);
    assert!(
        fixture.daemon.wait_for_phase(
            "workspace.stop",
            "cleanup_resources",
            Duration::from_secs(20)
        ),
        "the stop never reached its cleanup phase, so killing here would test nothing"
    );
    fixture.daemon.kill();
    reap(child);
    fixture.daemon.start();

    assert_recovered(&fixture);
}

/// A daemon killed while a sandbox is stopping must leave a state the next start
/// recovers from.
///
/// A sandbox stop unmounts the rootfs and removes the sandbox cgroup, and it records
/// both before doing them. A crash between the two leaves a sandbox whose record says
/// stopping, and recovery completes the stop rather than reporting a sandbox that is
/// half way down.
#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn a_daemon_killed_during_a_sandbox_stop_recovers() {
    if !root_only() {
        return;
    }

    let mut fixture = CrashFixture::new("sandbox-stop");
    let child = fixture.spawn(&["stop", &fixture.sandbox_name]);
    assert!(
        fixture
            .daemon
            .wait_for_phase("sandbox.stop", "unmount_rootfs", Duration::from_secs(20)),
        "the sandbox stop never reached its unmount phase, so killing here would test nothing"
    );
    fixture.daemon.kill();
    reap(child);
    fixture.daemon.start();

    // The sandbox is the unit here, so the assertion is about the sandbox: it has to be
    // in a settled state, and its cgroup and rootfs mount have to match that state.
    let status = fixture.daemon.cli(&["status", &fixture.sandbox_name]);
    let stdout = String::from_utf8_lossy(&status.stdout).into_owned();
    assert!(
        stdout.contains("status: stopped") || stdout.contains("status: running"),
        "the sandbox is not in a settled state: {stdout}{}",
        String::from_utf8_lossy(&status.stderr)
    );
    let sandbox_id = fixture.sandbox_id();
    let cgroup = Path::new("/sys/fs/cgroup").join(format!("enclave-sb-{sandbox_id}"));
    if stdout.contains("status: stopped") {
        assert!(
            !cgroup.exists(),
            "a stopped sandbox still has its cgroup {}",
            cgroup.display()
        );
    }
    assert_recovered(&fixture);
}
