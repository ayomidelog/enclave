//! A daemon a test starts itself, so it can kill it or ask it to repair.
//!
//! The CLI is used rather than the library because the daemon is the process
//! under test: a crash has to take the thing that owns the state with it, and
//! an in-process call cannot be killed without taking the test with it.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use enclave::operation::load;

pub(crate) struct TestDaemon {
    binary: PathBuf,
    state_dir: PathBuf,
    socket: PathBuf,
    pid_file: PathBuf,
    running: bool,
}

impl TestDaemon {
    pub(crate) fn new(state_dir: &Path, socket_dir: &Path) -> Self {
        Self {
            binary: enclave_binary(),
            state_dir: state_dir.to_path_buf(),
            socket: socket_dir.join("manager.sock"),
            pid_file: socket_dir.join("manager.pid"),
            running: false,
        }
    }

    /// Run one CLI command against this daemon and return its output.
    pub(crate) fn cli(&self, args: &[&str]) -> std::process::Output {
        Command::new(&self.binary)
            .arg("--socket")
            .arg(&self.socket)
            .args(args)
            .output()
            .expect("run the enclave CLI")
    }

    /// Run one CLI command and require it to succeed.
    pub(crate) fn cli_ok(&self, args: &[&str]) {
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
    pub(crate) fn spawn(&self, args: &[&str]) -> std::process::Child {
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
    pub(crate) fn start(&mut self) {
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

    pub(crate) fn pid(&self) -> u32 {
        fs::read_to_string(&self.pid_file)
            .expect("the daemon writes its pid file")
            .trim()
            .parse()
            .expect("the pid file holds a pid")
    }

    /// Kill the daemon the way a machine losing power would: no signal it can catch.
    pub(crate) fn kill(&mut self) {
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
    pub(crate) fn phase_of(&self, kind: &str) -> Option<String> {
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
    pub(crate) fn wait_for_phase(&self, kind: &str, phase: &str, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.phase_of(kind).as_deref() == Some(phase) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        false
    }

    /// The last `lines` lines of the daemon's log.
    ///
    /// The log lives beside the socket and the fixture's guard removes it, so a
    /// test that wants to report what the daemon decided has to read it while the
    /// daemon is still running rather than after the guard has cleaned up.
    pub(crate) fn log_tail(&self, lines: usize) -> String {
        let path = self
            .socket
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("daemon.log");
        let Ok(raw) = fs::read_to_string(&path) else {
            return format!("<no daemon log at {}>", path.display());
        };
        let all: Vec<&str> = raw.lines().collect();
        let start = all.len().saturating_sub(lines);
        all[start..].join("\n")
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        if self.running {
            let _ = self.cli(&["daemon", "stop"]);
        }
    }
}

pub(crate) fn enclave_binary() -> PathBuf {
    if let Ok(path) = std::env::var("CARGO_BIN_EXE_enclave") {
        return PathBuf::from(path);
    }
    let exe = std::env::current_exe().expect("resolve the test executable");
    exe.parent()
        .and_then(Path::parent)
        .expect("resolve the debug directory")
        .join("enclave")
}
