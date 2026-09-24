use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::namespace_cache::{duplicate_for_child, raw_fds};
use super::process_matches;
use crate::workspace::types::WorkspaceMetadata;

pub(crate) const MAX_HELPER_OUTPUT_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct HelperKey {
    pid: u32,
    starttime_ticks: u64,
}

struct PersistentHelper {
    socket: PathBuf,
    auth_token: String,
    child: Child,
}

static HELPERS: OnceLock<Mutex<HashMap<HelperKey, Arc<Mutex<PersistentHelper>>>>> = OnceLock::new();

#[derive(Debug, Serialize)]
struct CommandRequest<'a> {
    auth_token: &'a str,
    runtime_pid: u32,
    runtime_starttime_ticks: u64,
    sandbox_id: &'a str,
    workspace_id: &'a str,
    cwd: &'a str,
    command: &'a [String],
}

#[derive(Debug, Deserialize)]
struct CommandResponse {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

#[derive(Debug)]
pub(crate) struct PersistentCommandOutput {
    pub(crate) exit_code: i32,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

pub(crate) fn execute_persistent_command(
    workspace: &WorkspaceMetadata,
    cwd: &str,
    command: &[String],
) -> Result<PersistentCommandOutput> {
    let runtime_pid = workspace
        .runtime_pid
        .context("workspace has no runtime pid")?;
    let runtime_starttime_ticks = workspace
        .runtime_starttime_ticks
        .context("workspace has no runtime start time")?;
    if !process_matches(runtime_pid, Some(runtime_starttime_ticks)) {
        bail!(
            "workspace runtime pid {} is not alive or no longer matches expected start time",
            runtime_pid
        );
    }

    let key = HelperKey {
        pid: runtime_pid,
        starttime_ticks: runtime_starttime_ticks,
    };
    let helpers = HELPERS.get_or_init(|| Mutex::new(HashMap::new()));
    let helper = {
        let mut guard = helpers
            .lock()
            .map_err(|_| anyhow::anyhow!("persistent helper cache lock poisoned"))?;
        match guard.entry(key) {
            std::collections::hash_map::Entry::Occupied(entry) => Arc::clone(entry.get()),
            std::collections::hash_map::Entry::Vacant(entry) => {
                let helper = Arc::new(Mutex::new(start_helper(
                    workspace,
                    runtime_pid,
                    runtime_starttime_ticks,
                )?));
                entry.insert(Arc::clone(&helper));
                helper
            }
        }
    };
    let mut helper = helper
        .lock()
        .map_err(|_| anyhow::anyhow!("persistent helper lock poisoned"))?;
    if helper.child.try_wait()?.is_some() {
        drop(helper);
        if let Ok(mut guard) = helpers.lock() {
            guard.remove(&key);
        }
        bail!("persistent workspace session helper exited unexpectedly");
    }
    send_command(
        &mut helper,
        runtime_pid,
        runtime_starttime_ticks,
        workspace,
        cwd,
        command,
    )
}

fn start_helper(
    workspace: &WorkspaceMetadata,
    runtime_pid: u32,
    runtime_starttime_ticks: u64,
) -> Result<PersistentHelper> {
    let runtime_dir = Path::new(&workspace.workspace_path).join("runtime");
    fs::create_dir_all(&runtime_dir)
        .with_context(|| format!("failed to create {}", runtime_dir.display()))?;
    let socket_dir = Path::new("/run/enclave");
    fs::create_dir_all(socket_dir)
        .with_context(|| format!("failed to create {}", socket_dir.display()))?;
    fs::set_permissions(socket_dir, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("failed to secure {}", socket_dir.display()))?;
    let socket = socket_dir.join(format!(
        "session-{}-{}.sock",
        runtime_pid, runtime_starttime_ticks
    ));
    if let Err(error) = fs::remove_file(&socket) {
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(error).with_context(|| format!("failed to remove {}", socket.display()));
        }
    }
    let auth_token = Uuid::new_v4().to_string();
    let fds = duplicate_for_child(runtime_pid, runtime_starttime_ticks)?;
    let raw_fds = raw_fds(&fds);
    let pidfd = open_runtime_pidfd(runtime_pid)?;
    // The helper polls this pidfd to notice that its runtime is gone, so it has
    // to survive the `exec` that starts the helper.
    super::make_inheritable(&pidfd)?;
    let current_exe = super::resolve_session_helper_source();
    let helper_log = runtime_dir.join("session-helper.log");
    let helper_stderr = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&helper_log)
        .with_context(|| format!("failed to open {}", helper_log.display()))?;
    crate::perf::record_process_spawn();
    let mut args = vec![
        "internal".to_string(),
        "workspace-session-persistent-helper".to_string(),
        "--helper-socket".to_string(),
        socket
            .to_str()
            .context("helper socket path is not UTF-8")?
            .to_string(),
        "--runtime-pid".to_string(),
        runtime_pid.to_string(),
        "--runtime-starttime-ticks".to_string(),
        runtime_starttime_ticks.to_string(),
        "--runtime-pidfd".to_string(),
        pidfd.as_raw_fd().to_string(),
        "--sandbox-id".to_string(),
        workspace.sandbox_id.clone(),
        "--workspace-id".to_string(),
        workspace.id.clone(),
        "--auth-token".to_string(),
        auth_token.clone(),
    ];
    // Commands forked by the helper inherit its cgroup, so attaching the helper
    // here is what makes daemon-managed `workspace exec` respect the
    // workspace's declared CPU, memory, and process limits.
    if let Some(cgroup_path) =
        crate::workspace::existing_workspace_cgroup_path(&workspace.sandbox_id, &workspace.id)
    {
        args.push("--cgroup-path".to_string());
        args.push(cgroup_path);
    }
    for (name, fd) in [
        ("--root-fd", raw_fds[0]),
        ("--user-ns-fd", raw_fds[1]),
        ("--mount-ns-fd", raw_fds[2]),
        ("--pid-ns-fd", raw_fds[3]),
        ("--net-ns-fd", raw_fds[4]),
        ("--uts-ns-fd", raw_fds[5]),
    ] {
        args.push(name.to_string());
        args.push(fd.to_string());
    }
    let mut child = Command::new(current_exe)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::from(helper_stderr))
        .spawn()
        .context("failed to spawn persistent workspace session helper")?;
    drop(fds);
    drop(pidfd);

    let start_timeout = crate::deadlines::helper_start().get();
    let readiness_pidfd = open_process_pidfd(child.id());
    let started = Instant::now();
    while started.elapsed() < start_timeout {
        if socket.exists() {
            if let Some(fd) = readiness_pidfd {
                unsafe { libc::close(fd) };
            }
            return Ok(PersistentHelper {
                socket,
                auth_token,
                child,
            });
        }
        if child
            .try_wait()
            .context("failed to inspect persistent helper")?
            .is_some()
        {
            break;
        }
        let remaining = start_timeout.saturating_sub(started.elapsed());
        wait_for_helper_progress(readiness_pidfd, remaining);
    }
    if let Some(fd) = readiness_pidfd {
        unsafe { libc::close(fd) };
    }
    let mut child = child;
    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_file(&socket);
    let log_tail = fs::read_to_string(&helper_log).unwrap_or_default();
    bail!(
        "persistent workspace session helper did not become ready; log: {}\n{}",
        helper_log.display(),
        log_tail
    )
}

fn open_process_pidfd(pid: u32) -> Option<i32> {
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) as i32 };
    (fd >= 0).then_some(fd)
}

fn open_runtime_pidfd(pid: u32) -> Result<std::fs::File> {
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error()).context("failed to open runtime pidfd");
    }
    Ok(unsafe { std::fs::File::from_raw_fd(fd as i32) })
}

fn wait_for_helper_progress(pidfd: Option<i32>, remaining: Duration) {
    let Some(pidfd) = pidfd else {
        return;
    };
    let mut descriptor = libc::pollfd {
        fd: pidfd,
        events: libc::POLLIN,
        revents: 0,
    };
    let timeout_ms = remaining.as_millis().clamp(1, 100) as i32;
    let result = unsafe { libc::poll(&mut descriptor, 1, timeout_ms) };
    if result < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
        tracing::debug!(
            "persistent helper readiness poll failed: {}",
            std::io::Error::last_os_error()
        );
    }
}

fn send_command(
    helper: &mut PersistentHelper,
    runtime_pid: u32,
    runtime_starttime_ticks: u64,
    workspace: &WorkspaceMetadata,
    cwd: &str,
    command: &[String],
) -> Result<PersistentCommandOutput> {
    let started = Instant::now();
    let mut stream = loop {
        match UnixStream::connect(&helper.socket) {
            Ok(stream) => break stream,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                ) && started.elapsed() < Duration::from_secs(1) =>
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to connect to {}", helper.socket.display()))
            }
        }
    };
    let request = CommandRequest {
        auth_token: &helper.auth_token,
        runtime_pid,
        runtime_starttime_ticks,
        sandbox_id: &workspace.sandbox_id,
        workspace_id: &workspace.id,
        cwd,
        command,
    };
    serde_json::to_writer(&mut stream, &request).context("failed to encode persistent command")?;
    stream
        .write_all(b"\n")
        .context("failed to terminate persistent command")?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .context("failed to finish persistent command request")?;
    let mut response_line = String::new();
    BufReader::new(stream)
        .read_line(&mut response_line)
        .context("failed to read persistent command response")?;
    if response_line.is_empty() {
        bail!("persistent workspace session helper closed the connection")
    }
    let response: CommandResponse = serde_json::from_str(&response_line)
        .context("failed to decode persistent command response")?;
    Ok(PersistentCommandOutput {
        exit_code: response.exit_code,
        stdout: response.stdout,
        stderr: response.stderr,
    })
}

pub(crate) fn invalidate(pid: u32, starttime_ticks: u64) {
    let Some(helpers) = HELPERS.get() else {
        return;
    };
    if let Ok(mut guard) = helpers.lock() {
        if let Some(helper) = guard.remove(&HelperKey {
            pid,
            starttime_ticks,
        }) {
            if let Ok(mut helper) = helper.lock() {
                let _ = helper.child.kill();
                let _ = helper.child.wait();
                let _ = fs::remove_file(&helper.socket);
            }
        }
    }
}

pub(crate) fn output_to_string(bytes: Vec<u8>) -> String {
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::output_to_string;

    #[test]
    fn persistent_output_decodes_lossy_utf8() {
        assert_eq!(output_to_string(vec![b'o', b'k']), "ok");
    }
}
