use super::*;

pub(crate) fn wait_child_output(
    child: &mut ChildGuard,
    client_stream: Option<&UnixStream>,
) -> Result<Output> {
    let mut stderr_reader = child.child.stderr.take().map(|mut pipe| {
        thread::spawn(move || {
            let mut stderr = Vec::new();
            pipe.read_to_end(&mut stderr).map(|_| stderr)
        })
    });
    let status = wait_for_child_or_disconnect(child, client_stream, &mut stderr_reader)?;
    let stderr = match stderr_reader {
        Some(reader) => reader
            .join()
            .map_err(|_| anyhow!("child stderr reader panicked"))??,
        None => Vec::new(),
    };
    child.disarm();
    Ok(Output {
        status,
        stdout: Vec::new(),
        stderr,
    })
}

pub(crate) fn wait_for_child_or_disconnect(
    child: &mut ChildGuard,
    client_stream: Option<&UnixStream>,
    stderr_reader: &mut Option<thread::JoinHandle<std::io::Result<Vec<u8>>>>,
) -> Result<std::process::ExitStatus> {
    if client_stream.is_none() {
        return child
            .child
            .wait()
            .context("failed to wait for child process");
    }

    if let Some(pidfd) = crate::pidfd::open_pidfd(child.child.id()) {
        let client_fd = client_stream
            .expect("client stream checked above")
            .as_raw_fd();
        let mut descriptors = [
            libc::pollfd {
                fd: pidfd,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: client_fd,
                events: libc::POLLIN | libc::POLLRDHUP | libc::POLLHUP | libc::POLLERR,
                revents: 0,
            },
        ];
        loop {
            let poll_result = unsafe { libc::poll(descriptors.as_mut_ptr(), 2, -1) };
            if poll_result < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                unsafe { libc::close(pidfd) };
                return Err(error).context("failed waiting for child or client disconnect");
            }
            if client_disconnected(client_stream.expect("client stream checked above")) {
                unsafe { libc::close(pidfd) };
                let _ = child.child.kill();
                let _ = child.child.wait();
                if let Some(reader) = stderr_reader.take() {
                    let _ = reader.join();
                }
                bail!("workspace request client disconnected; command cancelled");
            }
            if descriptors[0].revents & libc::POLLIN != 0 {
                unsafe { libc::close(pidfd) };
                return child.child.wait().context("failed to reap child process");
            }
        }
    }

    loop {
        match child
            .child
            .try_wait()
            .context("failed to poll child process")?
        {
            Some(status) => return Ok(status),
            None => {
                if client_stream.is_some_and(|stream| !client_is_connected(stream)) {
                    let _ = child.child.kill();
                    let _ = child.child.wait();
                    if let Some(reader) = stderr_reader.take() {
                        let _ = reader.join();
                    }
                    bail!("workspace request client disconnected; command cancelled")
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

pub(crate) fn set_pipe_capacity(fd: std::os::fd::RawFd) {
    let requested = 1024 * 1024;
    let result = unsafe { libc::fcntl(fd, libc::F_SETPIPE_SZ, requested) };
    if result < 0 {
        tracing::debug!(fd, error = ?std::io::Error::last_os_error(), "unable to enlarge transfer pipe");
    }
}

/// Whether a client has gone away while the daemon is still working.
///
/// A request line and its terminating newline are consumed before dispatch, so
/// the only readable event left on a healthy connection is end-of-file or an
/// error. Using this on a command connection is what lets the daemon stop work
/// its caller abandoned instead of holding a worker until a much later deadline.
pub(crate) fn client_disconnected(stream: &UnixStream) -> bool {
    let mut descriptor = libc::pollfd {
        fd: stream.as_raw_fd(),
        events: libc::POLLIN | libc::POLLRDHUP | libc::POLLHUP | libc::POLLERR,
        revents: 0,
    };
    let result = unsafe { libc::poll(&mut descriptor, 1, 0) };
    if result <= 0 {
        return false;
    }
    if descriptor.revents & (libc::POLLRDHUP | libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0
    {
        return true;
    }
    if descriptor.revents & libc::POLLIN == 0 {
        return false;
    }
    let mut byte = 0_u8;
    let read = unsafe {
        libc::recv(
            stream.as_raw_fd(),
            (&mut byte as *mut u8).cast(),
            1,
            libc::MSG_PEEK | libc::MSG_DONTWAIT,
        )
    };
    read == 0
}

pub(crate) fn client_is_connected(stream: &UnixStream) -> bool {
    !client_disconnected(stream)
}
