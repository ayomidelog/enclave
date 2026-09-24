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

    if let Some(pidfd) = open_pidfd(child.child.id()) {
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
                events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
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
            if descriptors[1].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
                unsafe { libc::close(pidfd) };
                let _ = child.child.kill();
                let _ = child.child.wait();
                if let Some(reader) = stderr_reader.take() {
                    let _ = reader.join();
                }
                bail!("workspace cp client disconnected; transfer cancelled");
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
                    bail!("workspace cp client disconnected; transfer cancelled")
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

pub(crate) fn open_pidfd(pid: u32) -> Option<i32> {
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::c_uint, 0) as i32 };
    (fd >= 0).then_some(fd)
}

pub(crate) fn set_pipe_capacity(fd: std::os::fd::RawFd) {
    let requested = 1024 * 1024;
    let result = unsafe { libc::fcntl(fd, libc::F_SETPIPE_SZ, requested) };
    if result < 0 {
        tracing::debug!(fd, error = ?std::io::Error::last_os_error(), "unable to enlarge transfer pipe");
    }
}

pub(crate) fn client_is_connected(stream: &UnixStream) -> bool {
    let mut descriptor = libc::pollfd {
        fd: stream.as_raw_fd(),
        events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
        revents: 0,
    };
    let result = unsafe { libc::poll(&mut descriptor, 1, 0) };
    if result < 0 {
        return true;
    }
    descriptor.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) == 0
}
