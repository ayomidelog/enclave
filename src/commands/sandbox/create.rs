//! Creating a sandbox, and streaming the bootstrap output while it runs.
//!
//! A cold bootstrap takes minutes and produces its output in a log file inside
//! the sandbox directory the daemon is creating. The request itself blocks for
//! that whole time, so the client runs it on its own thread and polls the
//! directory meanwhile, echoing what the bootstrap has written. Without that the
//! operator sees a silent command for minutes and cannot tell progress from a
//! hang.
//!
//! The monitor is best effort by design. It reads a path the daemon owns, so it
//! can race with directory creation or find a truncated line; any failure
//! disables the streaming and leaves the request alone, because losing the
//! commentary is not a reason to lose the sandbox.

use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime};

use anyhow::{bail, Result};
use serde_json::{json, Value};

use crate::cli::CreateArgs;
use crate::sandbox::SandboxMetadata;

use super::super::{daemon, print_operation_id, print_state_transition, send};

/// How often the client looks for new bootstrap output while it waits.
const BOOTSTRAP_POLL_INTERVAL: Duration = Duration::from_millis(250);

pub(crate) fn run_create(socket: &Path, args: CreateArgs) -> Result<()> {
    daemon::ensure_daemon_running(socket)?;
    let health = send(socket, "daemon.health", json!({}))?;
    let sandboxes_dir = health
        .get("state_dir")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .map(|state_dir| state_dir.join("sandboxes"));
    let mut monitor = BootstrapConsoleMonitor::new(sandboxes_dir)?;

    println!(
        "creating sandbox '{}' with suite '{}' (this may take several minutes)...",
        args.name, args.suite
    );

    let request = json!({
        "name": args.name,
        "suite": args.suite,
        "mirror": args.mirror,
        "bootstrap_method": args.bootstrap_method.to_string(),
        "memory_mb": args.memory_mb,
        "cpu_percent": args.cpu_percent,
        "max_procs": args.max_procs,
    });
    // The create request blocks for the whole bootstrap, so it runs on its own
    // thread and this thread polls the log the bootstrap is writing.
    let socket_path = socket.to_path_buf();
    let (tx, rx) = mpsc::channel();
    let request_thread = thread::spawn(move || {
        let result = send(&socket_path, "sandbox.create", request);
        let _ = tx.send(result);
    });

    let response = loop {
        match rx.recv_timeout(BOOTSTRAP_POLL_INTERVAL) {
            Ok(result) => {
                monitor.poll();
                break result?;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => monitor.poll(),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                bail!("sandbox create request terminated unexpectedly")
            }
        }
    };
    if request_thread.join().is_err() {
        bail!("sandbox create request thread panicked");
    }
    let metadata: SandboxMetadata = serde_json::from_value(response.clone())?;

    println!("created and started sandbox {}", metadata.id);
    print_state_transition(&response);
    print_operation_id();
    println!("rootfs {}", metadata.rootfs_path);
    Ok(())
}

/// Echoes the bootstrap log of the sandbox the daemon is creating.
struct BootstrapConsoleMonitor {
    /// Where sandboxes live, from the daemon's own health report.
    sandboxes_dir: Option<PathBuf>,
    /// Sandboxes that already existed, so the new one can be told apart.
    known_sandbox_ids: HashSet<String>,
    log_path: Option<PathBuf>,
    log_offset: u64,
    disabled: bool,
}

impl BootstrapConsoleMonitor {
    fn new(sandboxes_dir: Option<PathBuf>) -> Result<Self> {
        let known_sandbox_ids = match sandboxes_dir.as_deref() {
            Some(dir) => list_sandbox_ids(dir)?,
            None => HashSet::new(),
        };
        Ok(Self {
            sandboxes_dir,
            known_sandbox_ids,
            log_path: None,
            log_offset: 0,
            disabled: false,
        })
    }

    fn poll(&mut self) {
        if self.disabled {
            return;
        }
        if let Err(err) = self.poll_inner() {
            tracing::warn!("failed to stream bootstrap output: {err:#}");
            self.disabled = true;
        }
    }

    fn poll_inner(&mut self) -> Result<()> {
        self.discover_log_path()?;
        self.print_new_log_bytes()?;
        Ok(())
    }

    /// Find the new sandbox directory and start following its bootstrap log.
    ///
    /// The newest directory that was not there at startup is the one being
    /// created. Every candidate is recorded as known even though only one is
    /// followed, so a later poll does not mistake an already-seen directory for
    /// the new one.
    fn discover_log_path(&mut self) -> Result<()> {
        if self.log_path.is_some() {
            return Ok(());
        }
        let Some(sandboxes_dir) = self.sandboxes_dir.as_deref() else {
            return Ok(());
        };
        if !sandboxes_dir.exists() {
            return Ok(());
        }

        let mut candidates = Vec::new();
        for entry in std::fs::read_dir(sandboxes_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let id = entry.file_name().to_string_lossy().to_string();
            if self.known_sandbox_ids.contains(&id) {
                continue;
            }
            let modified = entry
                .metadata()
                .and_then(|meta| meta.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            candidates.push((id, entry.path(), modified));
        }

        if candidates.is_empty() {
            return Ok(());
        }

        candidates.sort_by(|a, b| b.2.cmp(&a.2));
        for (id, _, _) in &candidates {
            self.known_sandbox_ids.insert(id.clone());
        }

        let (_, sandbox_path, _) = candidates.remove(0);
        let log_path = sandbox_path.join("debootstrap.log");
        self.log_path = Some(log_path.clone());
        println!("bootstrap live output:");
        println!("{}", log_path.display());
        Ok(())
    }

    /// Print whatever the log has gained since the last poll.
    fn print_new_log_bytes(&mut self) -> Result<()> {
        let Some(log_path) = self.log_path.as_deref() else {
            return Ok(());
        };
        if !log_path.exists() {
            return Ok(());
        }

        let mut file = File::open(log_path)?;
        let len = file.metadata()?.len();
        // A log that got shorter was replaced, so start from its beginning
        // rather than skipping the bytes it now holds.
        if self.log_offset > len {
            self.log_offset = 0;
        }
        if len == self.log_offset {
            return Ok(());
        }

        file.seek(SeekFrom::Start(self.log_offset))?;
        let mut chunk = Vec::new();
        file.read_to_end(&mut chunk)?;
        self.log_offset = len;

        if !chunk.is_empty() {
            let text = String::from_utf8_lossy(&chunk);
            print!("{text}");
            std::io::stdout().flush()?;
        }
        Ok(())
    }
}

fn list_sandbox_ids(sandboxes_dir: &Path) -> Result<HashSet<String>> {
    let mut ids = HashSet::new();
    if !sandboxes_dir.exists() {
        return Ok(ids);
    }
    for entry in std::fs::read_dir(sandboxes_dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            ids.insert(entry.file_name().to_string_lossy().to_string());
        }
    }
    Ok(ids)
}
