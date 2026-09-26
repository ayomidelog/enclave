//! Joining another process's network namespace from a child before exec.
//!
//! This runs between fork and exec, so it may only call async-signal-safe
//! functions: open, setns, and close qualify. Allocation and any Rust-level error
//! formatting do not, which is why the failure is returned as a raw errno.

pub(super) fn enter_network_namespace(pid: u32) -> std::io::Result<()> {
    let mut path = [0u8; 32];
    let mut length = 0usize;
    for byte in b"/proc/" {
        path[length] = *byte;
        length += 1;
    }
    // Render the pid without formatting machinery.
    let mut digits = [0u8; 10];
    let mut digit_count = 0usize;
    let mut value = pid;
    if value == 0 {
        digits[0] = b'0';
        digit_count = 1;
    } else {
        while value > 0 {
            digits[digit_count] = b'0' + (value % 10) as u8;
            digit_count += 1;
            value /= 10;
        }
    }
    for index in (0..digit_count).rev() {
        path[length] = digits[index];
        length += 1;
    }
    for byte in b"/ns/net" {
        path[length] = *byte;
        length += 1;
    }
    path[length] = 0;

    let fd = unsafe { libc::open(path.as_ptr().cast(), libc::O_RDONLY | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let result = unsafe { libc::setns(fd, libc::CLONE_NEWNET) };
    let error = std::io::Error::last_os_error();
    unsafe { libc::close(fd) };
    if result != 0 {
        return Err(error);
    }
    Ok(())
}
