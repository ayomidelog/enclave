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
