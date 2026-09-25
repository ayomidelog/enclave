//! Running one command inside the workspace the helper is holding open.
//!
//! The helper already has the namespaces, so a command is a fork and an exec rather
//! than a second entry into them. Its output is drained while it runs, because a pipe
//! that fills would block the child.

use super::*;

pub(super) fn handle_persistent_command(
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

pub(super) fn set_nonblocking(fd: i32) -> Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error()).context("failed to inspect pipe flags");
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error()).context("failed to set pipe nonblocking");
    }
    Ok(())
}
