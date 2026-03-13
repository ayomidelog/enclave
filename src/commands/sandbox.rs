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
use crate::sandbox::{SandboxListItem, SandboxMetadata, SandboxStatusReport};

use super::{confirm_destructive_action, daemon, send, send_managed};

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
    let socket_path = socket.to_path_buf();
    let (tx, rx) = mpsc::channel();
    let request_thread = thread::spawn(move || {
        let result = send(&socket_path, "sandbox.create", request);
        let _ = tx.send(result);
    });

    let response = loop {
        match rx.recv_timeout(Duration::from_millis(250)) {
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
    let metadata: SandboxMetadata = serde_json::from_value(response)?;

    println!("created and started sandbox {}", metadata.id);
    println!("rootfs {}", metadata.rootfs_path);
    Ok(())
}

struct BootstrapConsoleMonitor {
    sandboxes_dir: Option<PathBuf>,
    known_sandbox_ids: HashSet<String>,
    log_path: Option<PathBuf>,
    log_offset: u64,
    announced: bool,
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
            announced: false,
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

    fn print_new_log_bytes(&mut self) -> Result<()> {
        let Some(log_path) = self.log_path.as_deref() else {
            return Ok(());
        };
        if !log_path.exists() {
            return Ok(());
        }

        let mut file = File::open(log_path)?;
        let len = file.metadata()?.len();
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

        if !self.announced {
            self.announced = true;
        }
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

pub(crate) fn run_list(socket: &Path) -> Result<()> {
    let response = send_managed(socket, "sandbox.list", json!({}))?;
    let sandboxes: Vec<SandboxListItem> = serde_json::from_value(response)?;

    if sandboxes.is_empty() {
        println!("no sandboxes");
        return Ok(());
    }

    let id_width = sandboxes
        .iter()
        .map(|s| s.id.len())
        .max()
        .unwrap_or(2)
        .max("ID".len());
    let name_width = sandboxes
        .iter()
        .map(|s| s.name.len())
        .max()
        .unwrap_or(4)
        .max("NAME".len());
    let status_width = sandboxes
        .iter()
        .map(|s| format!("{:?}", s.status).to_lowercase().len())
        .max()
        .unwrap_or(6)
        .max("STATUS".len());

    println!(
        "{:<id_width$} {:<name_width$} {:<status_width$} WORKSPACES",
        "ID", "NAME", "STATUS"
    );
    for sandbox in sandboxes {
        println!(
            "{:<id_width$} {:<name_width$} {:<status_width$} {}",
            sandbox.id,
            sandbox.name,
            format!("{:?}", sandbox.status).to_lowercase(),
            sandbox.workspace_count
        );
    }

    Ok(())
}

pub(crate) fn run_remove(socket: &Path, sandbox_id: &str) -> Result<()> {
    tracing::info!("removing sandbox '{}'...", sandbox_id);
    send_managed(
        socket,
        "sandbox.remove",
        json!({ "sandbox_id": sandbox_id }),
    )?;
    println!("removed sandbox '{}'", sandbox_id);
    Ok(())
}

pub(crate) fn run_wipe(socket: &Path) -> Result<()> {
    let response = send_managed(socket, "sandbox.list", json!({}))?;
    let sandboxes: Vec<SandboxListItem> = serde_json::from_value(response)?;
    if sandboxes.is_empty() {
        println!("no sandboxes");
        return Ok(());
    }

    if !confirm_destructive_action(
        &format!(
            "this will permanently delete all {} sandboxes and their workspaces.",
            sandboxes.len()
        ),
        "delete all sandboxes",
    )? {
        println!("aborted");
        return Ok(());
    }

    let total = sandboxes.len();
    for sandbox in sandboxes {
        tracing::info!("destroying sandbox '{}'...", sandbox.id);
        send_managed(socket, "sandbox.destroy", json!({ "sandbox": sandbox.id }))?;
    }
    println!("deleted {total} sandboxes");
    Ok(())
}

pub(crate) fn run_start(socket: &Path, sandbox: &str) -> Result<()> {
    tracing::info!("starting sandbox '{}'...", sandbox);
    send_managed(socket, "sandbox.start", json!({ "sandbox": sandbox }))?;
    println!("started sandbox '{}'", sandbox);
    Ok(())
}

pub(crate) fn run_stop(socket: &Path, sandbox: &str) -> Result<()> {
    tracing::info!("stopping sandbox '{}'...", sandbox);
    send_managed(socket, "sandbox.stop", json!({ "sandbox": sandbox }))?;
    println!("stopped sandbox '{}'", sandbox);
    Ok(())
}

pub(crate) fn run_destroy(socket: &Path, sandbox: &str) -> Result<()> {
    tracing::info!("destroying sandbox '{}'...", sandbox);
    send_managed(socket, "sandbox.destroy", json!({ "sandbox": sandbox }))?;
    println!("destroyed sandbox '{}'", sandbox);
    Ok(())
}

pub(crate) fn run_status(socket: &Path, sandbox: &str) -> Result<()> {
    let response = send_managed(socket, "sandbox.status", json!({ "sandbox": sandbox }))?;
    let status: SandboxStatusReport = serde_json::from_value(response)?;
    println!("id: {}", status.id);
    println!("name: {}", status.name);
    println!("created_at: {}", status.created_at);
    println!("status: {}", format!("{:?}", status.status).to_lowercase());
    println!("rootfs_path: {}", status.rootfs_path);
    println!(
        "rootfs_disk_usage_bytes: {}",
        status.rootfs_disk_usage_bytes
    );
    println!("workspace_count: {}", status.workspace_count);
    if let Some(cpu_percent) = status.limits.cpu_percent {
        println!(
            "cpu_limit: {}",
            crate::resource_limits::format_cpu_percent(cpu_percent)
        );
    }
    println!(
        "memory_limit_bytes: {}",
        status
            .limits
            .memory_bytes
            .map_or_else(|| "unlimited".to_string(), |v| v.to_string())
    );
    println!(
        "max_processes: {}",
        status
            .limits
            .max_processes
            .map_or_else(|| "unlimited".to_string(), |v| v.to_string())
    );
    Ok(())
}
