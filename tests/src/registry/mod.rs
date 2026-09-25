//! Tests for the registry.
//!
//! The fixture is here because the submodules share it: a file whose modification time
//! is set back, which is how a record that predates a migration is built.

use super::*;
use std::fs;

/// Move a path's modification time `seconds` into the past.
fn backdate(path: &std::path::Path, seconds: i64) {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let raw = CString::new(path.as_os_str().as_bytes()).expect("path is not NUL-terminated");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock is after the epoch")
        .as_secs() as i64;
    let stamp = libc::timespec {
        tv_sec: now - seconds,
        tv_nsec: 0,
    };
    let result =
        unsafe { libc::utimensat(libc::AT_FDCWD, raw.as_ptr(), [stamp, stamp].as_ptr(), 0) };
    assert_eq!(result, 0, "failed to backdate {}", path.display());
}

mod disagreement;
mod migration;
mod repair;
