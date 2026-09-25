//! The persistent workspace session helper.
//!
//! The helper holds a workspace namespaces open so a command does not pay for
//! entering them on every call, and it serves those commands over a socket for the
//! lifetime of the runtime it was started for. This module owns the helper process:
//! its socket, the loop that waits for work, and the check that the runtime it serves
//! is still alive. Running one command is in the command module.

use super::*;

#[derive(Debug, Deserialize)]
struct PersistentCommandRequest {
    auth_token: String,
    runtime_pid: u32,
    runtime_starttime_ticks: u64,
    sandbox_id: String,
    workspace_id: String,
    cwd: String,
    command: Vec<String>,
}

#[derive(Debug, Serialize)]
struct PersistentCommandResponse {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

pub(crate) fn run_workspace_session_persistent_helper(
    args: WorkspaceSessionPersistentHelperArgs,
) -> Result<()> {
    if !crate::workspace::session_process_matches(
        args.runtime_pid,
        Some(args.runtime_starttime_ticks),
    ) {
        bail!("workspace runtime is no longer alive or has changed identity");
    }
    attach_helper_to_workspace_cgroup(&args.cgroup_path)?;
    let socket_path = Path::new(&args.helper_socket);
    if let Some(parent) = socket_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    if let Err(error) = fs::remove_file(socket_path) {
        if error.kind() != std::io::ErrorKind::NotFound {
            return Err(error)
                .with_context(|| format!("failed to remove {}", socket_path.display()));
        }
    }
    let listener = UnixListener::bind(socket_path)
        .with_context(|| format!("failed to bind {}", socket_path.display()))?;
    fs::set_permissions(socket_path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to secure {}", socket_path.display()))?;

    let namespaces = NamespaceHandles::from_optional_fds(
        args.runtime_pid,
        [
            Some(args.root_fd),
            Some(args.user_ns_fd),
            Some(args.mount_ns_fd),
            Some(args.pid_ns_fd),
            Some(args.net_ns_fd),
            Some(args.uts_ns_fd),
        ],
    )?;
    enter_workspace_namespaces(&namespaces)?;

    loop {
        // Wait for a command, but wake up periodically to notice that the
        // runtime this helper serves is gone. Without the timeout the helper
        // would sit in `accept()` forever after a daemon crash, holding its
        // mount namespace and the workspace cgroup with it.
        let mut descriptor = libc::pollfd {
            fd: listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut descriptor, 1, PERSISTENT_HELPER_IDLE_TIMEOUT_MS) };
        if ready < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error).context("persistent helper poll failed");
        }
        if ready == 0 {
            if !runtime_pidfd_alive(args.runtime_pidfd) {
                return Ok(());
            }
            continue;
        }
        let stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error).context("persistent helper accept failed"),
        };
        if let Err(error) = handle_persistent_command(&args, &namespaces.root_dir, stream) {
            eprintln!("enclave: persistent command rejected: {error:#}");
        }
    }
}

/// How long the persistent helper waits for a command before re-checking that
/// its runtime is still alive.
const PERSISTENT_HELPER_IDLE_TIMEOUT_MS: libc::c_int = 5_000;

mod command;

use command::handle_persistent_command;
/// Whether the runtime this helper serves is still alive.
///
/// Liveness is read from the inherited pidfd rather than `/proc`: the helper
/// lives inside the workspace's PID namespace, where the host PID of its runtime
/// does not resolve, so a `/proc` lookup there would always report the runtime as
/// gone. `pidfd_open` sets `FD_CLOEXEC`, so the caller clears it before `exec`;
/// without that the descriptor number is closed by `exec` and may be reused,
/// which would make this check report a live runtime forever.
fn runtime_pidfd_alive(pidfd: i32) -> bool {
    if pidfd < 0 {
        return false;
    }
    // A pidfd reports readability only for the events the caller asked about,
    // so `events` must include POLLIN for an exited runtime to be visible here.
    let mut descriptor = libc::pollfd {
        fd: pidfd,
        events: libc::POLLIN,
        revents: 0,
    };
    let result = unsafe { libc::poll(&mut descriptor, 1, 0) };
    if result < 0 {
        return false;
    }
    // POLLNVAL means the descriptor is not a pidfd here, which must be treated
    // as "cannot prove the runtime is alive" rather than "alive".
    descriptor.revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) == 0
}

fn create_pipe() -> Result<(File, File)> {
    let mut fds = [0; 2];
    let result = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
    if result != 0 {
        return Err(std::io::Error::last_os_error()).context("failed to create command pipe");
    }
    Ok((unsafe { File::from_raw_fd(fds[0]) }, unsafe {
        File::from_raw_fd(fds[1])
    }))
}

fn redirect_pipe_to_stdio(writer: File, target_fd: i32) -> Result<()> {
    let result = unsafe { libc::dup2(writer.as_raw_fd(), target_fd) };
    if result < 0 {
        return Err(std::io::Error::last_os_error()).context("failed to redirect command output");
    }
    Ok(())
}
