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

fn handle_persistent_command(
    args: &WorkspaceSessionPersistentHelperArgs,
    root_dir: &File,
    mut stream: UnixStream,
) -> Result<()> {
    let mut line = String::new();
    BufReader::new(&mut stream)
        .read_line(&mut line)
        .context("failed to read persistent command request")?;
    let request: PersistentCommandRequest =
        serde_json::from_str(&line).context("invalid persistent command request")?;
    if request.auth_token != args.auth_token
        || request.runtime_pid != args.runtime_pid
        || request.runtime_starttime_ticks != args.runtime_starttime_ticks
        || request.sandbox_id != args.sandbox_id
        || request.workspace_id != args.workspace_id
    {
        bail!("persistent command identity validation failed");
    }
    if request.command.is_empty() || request.command.len() > 256 {
        bail!("persistent command has an invalid argument count");
    }
    if !runtime_pidfd_alive(args.runtime_pidfd) {
        bail!("workspace runtime pidfd is no longer alive");
    }
    let cwd = crate::workspace::sanitize_workspace_cwd(&request.cwd);
    let (stdout_reader, stdout_writer) = create_pipe()?;
    let (stderr_reader, stderr_writer) = create_pipe()?;
    let child = unsafe { fork() }.context("failed to fork persistent command")?;
    match child {
        ForkResult::Parent { child } => {
            drop(stdout_writer);
            drop(stderr_writer);
            let (code, stdout, stderr) = drain_command_pipes(stdout_reader, stderr_reader, child)?;
            let response = PersistentCommandResponse {
                exit_code: code,
                stdout: crate::workspace::session::output_to_string(stdout),
                stderr: crate::workspace::session::output_to_string(stderr),
            };
            serde_json::to_writer(&mut stream, &response)
                .context("failed to write persistent command response")?;
            stream
                .write_all(b"\n")
                .context("failed to terminate persistent command response")?;
            Ok(())
        }
        ForkResult::Child => {
            redirect_pipe_to_stdio(stdout_writer, libc::STDOUT_FILENO)?;
            redirect_pipe_to_stdio(stderr_writer, libc::STDERR_FILENO)?;
            if let Err(error) = run_workspace_command_child(
                root_dir,
                &cwd,
                &request.sandbox_id,
                &request.workspace_id,
                &request.command,
            ) {
                eprintln!("enclave: {error:#}");
                std::process::exit(1);
            }
            unreachable!("persistent command child should exec or exit");
        }
    }
}

fn drain_command_pipes(
    stdout_reader: File,
    stderr_reader: File,
    child: Pid,
) -> Result<(i32, Vec<u8>, Vec<u8>)> {
    set_nonblocking(stdout_reader.as_raw_fd())?;
    set_nonblocking(stderr_reader.as_raw_fd())?;
    let mut stdout_reader = Some(stdout_reader);
    let mut stderr_reader = Some(stderr_reader);
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut child_status = None;
    while child_status.is_none() || stdout_reader.is_some() || stderr_reader.is_some() {
        let mut descriptors = Vec::with_capacity(2);
        if let Some(reader) = stdout_reader.as_ref() {
            descriptors.push(libc::pollfd {
                fd: reader.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            });
        }
        if let Some(reader) = stderr_reader.as_ref() {
            descriptors.push(libc::pollfd {
                fd: reader.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            });
        }
        if !descriptors.is_empty() {
            let result =
                unsafe { libc::poll(descriptors.as_mut_ptr(), descriptors.len() as _, 100) };
            if result < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() != std::io::ErrorKind::Interrupted {
                    return Err(error).context("failed to poll persistent command output");
                }
            }
        }
        if let Some(reader) = stdout_reader.as_ref() {
            if descriptors
                .first()
                .is_some_and(|descriptor| descriptor.revents != 0)
                && drain_pipe(reader, &mut stdout)?
            {
                stdout_reader = None;
            }
        }
        let stderr_ready = if stdout_reader.is_some() {
            descriptors
                .get(1)
                .is_some_and(|descriptor| descriptor.revents != 0)
        } else {
            descriptors
                .first()
                .is_some_and(|descriptor| descriptor.revents != 0)
        };
        if let Some(reader) = stderr_reader.as_ref() {
            if stderr_ready && drain_pipe(reader, &mut stderr)? {
                stderr_reader = None;
            }
        }
        if child_status.is_none() {
            match waitpid(child, Some(WaitPidFlag::WNOHANG))
                .context("failed to poll command child")?
            {
                WaitStatus::Exited(_, code) => child_status = Some(code),
                WaitStatus::Signaled(_, signal, _) => child_status = Some(128 + signal as i32),
                WaitStatus::StillAlive => {}
                _ => {}
            }
        }
    }
    Ok((child_status.unwrap_or(1), stdout, stderr))
}

fn drain_pipe(reader: &File, output: &mut Vec<u8>) -> Result<bool> {
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let amount =
            unsafe { libc::read(reader.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len()) };
        if amount > 0 {
            let amount = amount as usize;
            if output.len() < crate::workspace::session::MAX_HELPER_OUTPUT_BYTES {
                let remaining = crate::workspace::session::MAX_HELPER_OUTPUT_BYTES - output.len();
                output.extend_from_slice(&buffer[..amount.min(remaining)]);
            }
            continue;
        }
        if amount == 0 {
            return Ok(true);
        }
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        if error.kind() == std::io::ErrorKind::WouldBlock {
            return Ok(false);
        }
        return Err(error).context("failed to read persistent command output");
    }
}

fn set_nonblocking(fd: i32) -> Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error()).context("failed to inspect pipe flags");
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error()).context("failed to set pipe nonblocking");
    }
    Ok(())
}

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
