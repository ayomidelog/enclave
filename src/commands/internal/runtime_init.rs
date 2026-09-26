use std::fs::File;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicI32, Ordering};

use anyhow::{Context, Result};
use nix::sys::signal::{self, SaFlags, SigAction, SigHandler, SigSet, Signal};
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};

/// Write end of the shutdown self-pipe, published for the signal handler.
///
/// The handler runs with an async-signal-safe context, so it can only write a
/// byte and return. Everything else happens in the main loop.
static SHUTDOWN_FD: AtomicI32 = AtomicI32::new(-1);

extern "C" fn request_shutdown(_signal: i32) {
    let fd = SHUTDOWN_FD.load(Ordering::Relaxed);
    if fd >= 0 {
        let byte = [1u8];
        // The result is deliberately ignored: a signal handler cannot report
        // errors, and a failed write only means the loop already exited.
        unsafe { libc::write(fd, byte.as_ptr().cast(), byte.len()) };
    }
}

/// Stay alive as the init process of the workspace PID namespace.
///
/// The runtime is PID 1 inside its own PID namespace, where the kernel ignores
/// signals that have no handler. Without explicit handlers a `SIGTERM` from the
/// daemon does nothing, so every stop had to wait out the graceful-shutdown
/// budget and fall back to a forced kill. Handling the signals makes the
/// graceful path work, and reaping children keeps orphaned descendants from
/// accumulating as zombies under init.
pub(super) fn run_runtime_init_loop() -> Result<()> {
    let (read_end, write_end) = shutdown_pipe()?;
    install_shutdown_handlers()?;
    SHUTDOWN_FD.store(write_end.as_raw_fd(), Ordering::Relaxed);
    // The handler needs the write end for the rest of the runtime's life, so it
    // is held here until this function returns.
    let _write_end_guard = write_end;
    let mut read_end = read_end;
    let mut buffer = [0u8; 1];
    loop {
        reap_children();
        let mut descriptor = libc::pollfd {
            fd: read_end.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let result = unsafe { libc::poll(&mut descriptor, 1, SHUTDOWN_POLL_MS) };
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error).context("workspace runtime shutdown poll failed");
        }
        if result == 0 {
            continue;
        }
        if read_end.read(&mut buffer).is_ok() {
            return Ok(());
        }
    }
}

const SHUTDOWN_POLL_MS: i32 = 1_000;

fn shutdown_pipe() -> Result<(File, OwnedFd)> {
    let mut fds = [0i32; 2];
    let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error()).context("failed to create shutdown pipe");
    }
    // Both ends are owned by this process; the read end is used by the loop and
    // the write end is kept alive for the signal handler.
    let read_end = unsafe { File::from_raw_fd(fds[0]) };
    let write_end = unsafe { OwnedFd::from_raw_fd(fds[1]) };
    Ok((read_end, write_end))
}

fn install_shutdown_handlers() -> Result<()> {
    let action = SigAction::new(
        SigHandler::Handler(request_shutdown),
        SaFlags::SA_RESTART,
        SigSet::empty(),
    );
    for signal_number in [
        Signal::SIGTERM,
        Signal::SIGINT,
        Signal::SIGQUIT,
        Signal::SIGHUP,
    ] {
        // SAFETY: the handler only performs an async-signal-safe write.
        unsafe { signal::sigaction(signal_number, &action) }
            .with_context(|| format!("failed to install {signal_number} handler"))?;
    }
    Ok(())
}

fn reap_children() {
    loop {
        match waitpid(nix::unistd::Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::StillAlive) | Err(_) => return,
            Ok(_) => continue,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutdown_pipe_is_close_on_exec_and_usable() {
        let (read_end, write_end) = shutdown_pipe().expect("create shutdown pipe");
        for fd in [read_end.as_raw_fd(), write_end.as_raw_fd()] {
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            assert!(flags >= 0);
            assert_eq!(flags & libc::FD_CLOEXEC, libc::FD_CLOEXEC);
        }
        let byte = [1u8];
        assert_eq!(
            unsafe { libc::write(write_end.as_raw_fd(), byte.as_ptr().cast(), 1) },
            1
        );
        let mut read_end = read_end;
        let mut buffer = [0u8; 1];
        assert!(read_end.read(&mut buffer).is_ok());
        assert_eq!(buffer[0], 1);
    }

    #[test]
    fn signal_handler_writes_to_the_published_descriptor() {
        let (read_end, write_end) = shutdown_pipe().expect("create shutdown pipe");
        SHUTDOWN_FD.store(write_end.as_raw_fd(), Ordering::Relaxed);
        request_shutdown(libc::SIGTERM);
        SHUTDOWN_FD.store(-1, Ordering::Relaxed);
        let mut read_end = read_end;
        let mut buffer = [0u8; 1];
        assert!(read_end.read(&mut buffer).is_ok());
        assert_eq!(buffer[0], 1);
    }
}
