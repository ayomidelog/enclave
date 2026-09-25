//! Starting the persistent helper and waiting for its socket.

use std::fs::{self, OpenOptions};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use uuid::Uuid;

use super::super::namespace_cache::{duplicate_for_child, raw_fds};
use super::PersistentHelper;

pub(super) fn start_helper(
    workspace: &crate::workspace::types::WorkspaceMetadata,
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
    // to survive the exec that starts the helper.
    super::super::make_inheritable(&pidfd)?;
    let current_exe = super::super::resolve_session_helper_source();
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
    // here is what makes a daemon-managed workspace exec respect the workspace's
    // declared CPU, memory, and process limits.
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
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(helper_stderr))
        .spawn()
        .context("failed to spawn persistent workspace session helper")?;
    drop(fds);
    drop(pidfd);

    let helper_start = crate::deadlines::helper_start();
    let start_timeout = helper_start.get();
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
        "persistent workspace session helper did not become ready within {}; log: {}\n{}",
        helper_start.describe_timeout(),
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

/// Wait until the helper either exits or the remaining deadline passes.
///
/// Polling the helper's pidfd is what keeps the readiness wait from spinning: the
/// pidfd becomes readable the moment the helper exits, and the timeout bounds the
/// wait otherwise.
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
