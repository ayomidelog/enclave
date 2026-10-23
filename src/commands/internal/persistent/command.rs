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
            // A foreground command must not leave background work behind; the
            // explicit background mode is the only way to keep a descendant.
            terminate_group_descendants();
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

pub(crate) fn drain_command_pipes(
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
    // Wait for the command itself, never for its pipes to reach EOF. A command is
    // free to leave a descendant behind that inherited stdout/stderr - a background
    // job, a started daemon, `nohup ... &` - and that descendant keeps the write
    // ends open long after the command exited. Waiting for EOF then waits forever,
    // which pinned the helper, the workspace's command mutex, and through them a
    // daemon exec worker. Every later command for that workspace queued behind it
    // until the whole exec pool was stuck and its queue overflowed.
    while child_status.is_none() {
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
        } else {
            // Both pipes are closed but the command is still running. Without a
            // sleep this branch spins on `poll` with no descriptors and burns a
            // core until the child exits.
            std::thread::sleep(std::time::Duration::from_millis(10));
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
    // The direct child is gone, so everything it wrote is already in the pipe
    // buffer. Drain that, give a descendant a brief moment to flush anything still
    // in flight, and return instead of blocking on an EOF that may never come.
    drain_after_exit(&mut stdout_reader, &mut stdout)?;
    drain_after_exit(&mut stderr_reader, &mut stderr)?;
    Ok((child_status.unwrap_or(1), stdout, stderr))
}

/// Drain what a pipe still holds after the command exited, then stop.
///
/// The wait is short and bounded because the command is already gone: its output
/// is in the buffer, and only a descendant that inherited the pipe could still be
/// writing. Truncating that descendant's later output is the correct trade - the
/// alternative is never returning at all.
fn drain_after_exit(reader: &mut Option<File>, output: &mut Vec<u8>) -> Result<()> {
    let Some(file) = reader.as_ref() else {
        return Ok(());
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(200);
    loop {
        let mut descriptor = libc::pollfd {
            fd: file.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Ok(());
        }
        let timeout_ms = remaining.as_millis().min(100) as libc::c_int;
        let ready = unsafe { libc::poll(&mut descriptor, 1, timeout_ms) };
        if ready < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error).context("failed to poll persistent command output");
        }
        if ready == 0 {
            continue;
        }
        // `drain_pipe` reads until the pipe is empty or closed.
        if drain_pipe(file, output)? {
            return Ok(());
        }
    }
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
