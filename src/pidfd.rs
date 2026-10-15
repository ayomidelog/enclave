//! Opening a pidfd, which every wait on a child process uses.
//!
//! A pidfd is a handle on one process rather than on a number, so a wait on it
//! cannot be answered by a different process that inherited the number. The three
//! waits that use one — a host command, a workspace copy helper, and a session stop —
//! all open it the same way, so the call lives here.

/// A handle on `pid`, or `None` on a kernel that does not provide one.
///
/// `pidfd_open` is not a syscall every kernel has, and its absence is not a failure:
/// every caller falls back to checking the process itself, which is what they did
/// before pidfds existed.
pub(crate) fn open_pidfd(pid: u32) -> Option<i32> {
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) as i32 };
    (fd >= 0).then_some(fd)
}
