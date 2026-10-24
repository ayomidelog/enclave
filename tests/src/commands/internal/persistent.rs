use crate::commands::internal::persistent::{
    create_pipe, drain_command_pipes, redirect_pipe_to_stdio,
};
use nix::unistd::{fork, ForkResult};

/// A command that leaves a background descendant behind must not hang the drain.
///
/// This is the shape that froze a live host: the command exits at once but a
/// descendant it started keeps the inherited stdout pipe open, so waiting for EOF
/// waited until the descendant died. That pinned the helper, every daemon exec
/// worker that later asked for the same workspace, and finally the whole exec
/// queue, which then rejected requests with "queue is full".
#[test]
fn a_descendant_holding_stdout_does_not_hang_the_drain() {
    let (stdout_reader, stdout_writer) = create_pipe().expect("stdout pipe");
    let (stderr_reader, stderr_writer) = create_pipe().expect("stderr pipe");
    let child = unsafe { fork() }.expect("fork");
    match child {
        ForkResult::Parent { child } => {
            drop(stdout_writer);
            drop(stderr_writer);
            let started = std::time::Instant::now();
            let (code, stdout, _stderr, drain) =
                drain_command_pipes(stdout_reader, stderr_reader, child).expect("drain");
            let elapsed = started.elapsed();
            assert_eq!(code, 0);
            assert_eq!(String::from_utf8_lossy(&stdout).trim(), "before exit");
            assert!(drain.timed_out, "the descendant still held the pipe open");
            assert!(
                elapsed < std::time::Duration::from_secs(2),
                "drain waited {elapsed:?} for an EOF a descendant was holding open"
            );
        }
        ForkResult::Child => {
            if redirect_pipe_to_stdio(stdout_writer, libc::STDOUT_FILENO).is_err()
                || redirect_pipe_to_stdio(stderr_writer, libc::STDERR_FILENO).is_err()
            {
                unsafe { libc::_exit(1) }
            }
            // The grandchild inherits fd 1 and outlives the command, holding the
            // write end of the pipe open. It exits on its own so the test leaves
            // nothing behind.
            if let Ok(ForkResult::Child) = unsafe { fork() } {
                unsafe { libc::sleep(5) };
                unsafe { libc::_exit(0) }
            }
            let message = b"before exit\n";
            unsafe {
                libc::write(libc::STDOUT_FILENO, message.as_ptr().cast(), message.len());
            }
            unsafe { libc::_exit(0) }
        }
    }
}
