//! Mechanism for running one host command under a deadline with capped output.

use std::io::Read;
use std::process::Child;
use std::thread;
use std::time::{Duration, Instant};

/// How long to keep collecting output after the child has been stopped.
///
/// A reader thread ends when the last writer of the pipe closes it. Killing the
/// child closes the child's copy, so this grace only matters when a descendant
/// inherited the descriptor.
pub(super) const OUTPUT_GRACE: Duration = Duration::from_secs(2);

/// Poll interval used only when the kernel cannot provide a pidfd.
const FALLBACK_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Collect up to `cap` bytes from a pipe while always draining it.
///
/// Draining matters even after the cap is reached: a child that keeps writing
/// would otherwise block on a full pipe and never exit, which would turn a
/// bounded command into an unbounded one.
pub(super) fn read_capped<R: Read>(mut reader: R, cap: usize) -> Vec<u8> {
    let mut collected = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => return collected,
            Ok(read) => {
                if collected.len() < cap {
                    let remaining = cap - collected.len();
                    collected.extend_from_slice(&chunk[..read.min(remaining)]);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return collected,
        }
    }
}

/// Wait for the child to exit, returning whether it did so within `timeout`.
///
/// A pidfd makes this event driven, so a fast command is reaped as soon as the
/// kernel reports the exit instead of on the next poll tick. Kernels without
/// pidfd support fall back to polling `try_wait` inside the same budget.
pub(super) fn wait_until_deadline(child: &mut Child, timeout: Duration) -> std::io::Result<bool> {
    if let Some(pidfd) = open_pidfd(child.id()) {
        let ready = poll_pidfd(pidfd, timeout);
        unsafe { libc::close(pidfd) };
        if ready {
            // Reaping here is what makes the exit observable; `wait` later
            // returns the status this call cached.
            child.try_wait()?;
            return Ok(true);
        }
        return child.try_wait().map(|status| status.is_some());
    }

    let deadline = Instant::now() + timeout;
    loop {
        if child.try_wait()?.is_some() {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        thread::sleep(remaining.min(FALLBACK_POLL_INTERVAL));
    }
}

fn open_pidfd(pid: u32) -> Option<i32> {
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) as i32 };
    (fd >= 0).then_some(fd)
}

/// Poll a pidfd for exit, returning true when it became readable.
fn poll_pidfd(pidfd: i32, timeout: Duration) -> bool {
    let mut descriptor = libc::pollfd {
        fd: pidfd,
        events: libc::POLLIN,
        revents: 0,
    };
    let timeout_ms = timeout.as_millis().clamp(1, i32::MAX as u128) as i32;
    loop {
        let result = unsafe { libc::poll(&mut descriptor, 1, timeout_ms) };
        if result > 0 {
            return true;
        }
        if result == 0 {
            return false;
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return false;
        }
    }
}
