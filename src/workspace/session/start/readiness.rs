//! Waiting for a launched session to say it is up.
//!
//! The launcher writes a pid file and a ready file when it has finished setting
//! up its namespaces and mounts. Waiting is done on an inotify watch rather than
//! by polling, and a helper that cannot load at all is reported immediately
//! instead of after the deadline.

use super::*;

pub(crate) fn wait_for_session_ready(
    ready_file: &Path,
    pid_file: &Path,
    log_file: &Path,
    memory_limit: Option<u64>,
    deadline: crate::deadlines::Deadline,
) -> Result<()> {
    let timeout = deadline.get();
    let parent = ready_file
        .parent()
        .ok_or_else(|| anyhow::anyhow!("session ready path has no parent"))?;
    let parent_cstr = std::ffi::CString::new(parent.as_os_str().as_bytes())
        .context("session ready path contains an interior NUL byte")?;
    let inotify_fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) };
    if inotify_fd < 0 {
        return Err(std::io::Error::last_os_error()).context("failed to initialize inotify");
    }
    let watch = unsafe {
        libc::inotify_add_watch(
            inotify_fd,
            parent_cstr.as_ptr(),
            libc::IN_CLOSE_WRITE | libc::IN_CREATE | libc::IN_MOVED_TO,
        )
    };
    if watch < 0 {
        let error = std::io::Error::last_os_error();
        unsafe { libc::close(inotify_fd) };
        return Err(error).context("failed to watch session readiness directory");
    }

    let started = Instant::now();
    let mut events = [0u8; 4096];
    loop {
        if ready_file.exists() && pid_file.exists() {
            unsafe {
                libc::inotify_rm_watch(inotify_fd, watch);
                libc::close(inotify_fd);
            }
            return Ok(());
        }
        // The helper is a dynamically linked binary, so it can fail before it
        // runs any of its own code. Reporting that here rather than waiting out
        // the deadline is the difference between a failure that names the
        // library and the limit, and one that only says "did not become ready".
        if let Some(failure) = session_helper_load_failure(log_file, memory_limit) {
            unsafe {
                libc::inotify_rm_watch(inotify_fd, watch);
                libc::close(inotify_fd);
            }
            // The helper could not start at all, so this is the host not
            // providing what it needs rather than a wait that ran long.
            return Err(crate::error::coded(
                crate::error::ErrorCode::Unsupported,
                failure,
            ));
        }
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            unsafe {
                libc::inotify_rm_watch(inotify_fd, watch);
                libc::close(inotify_fd);
            }
            let tail = process::read_log_tail(log_file, 20).unwrap_or_default();
            let rendered_tail = if tail.trim().is_empty() {
                "<empty>".to_string()
            } else {
                tail
            };
            return Err(crate::error::coded(
                crate::error::ErrorCode::Timeout,
                format!(
                    "workspace session did not become ready within {} (expected files: {}, {}). log file: {}. recent log:\n{}",
                    deadline.describe_timeout(),
                    pid_file.display(),
                    ready_file.display(),
                    log_file.display(),
                    rendered_tail
                ),
            ));
        }

        let timeout_ms = remaining.as_millis().min(i32::MAX as u128) as i32;
        let mut descriptor = libc::pollfd {
            fd: inotify_fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let poll_result = unsafe { libc::poll(&mut descriptor, 1, timeout_ms) };
        if poll_result < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            unsafe {
                libc::inotify_rm_watch(inotify_fd, watch);
                libc::close(inotify_fd);
            }
            return Err(error).context("failed waiting for session readiness");
        }
        if poll_result > 0 {
            let _ = unsafe { libc::read(inotify_fd, events.as_mut_ptr().cast(), events.len()) };
        }
    }
}

/// Recognize a runtime helper that could not be loaded at all.
///
/// The loader reports the same message for a host whose libc is older than the
/// one the helper was built against and for a workspace memory limit too small to
/// map the shared objects the helper needs. Both leave a log line and no ready
/// file, so this turns the readiness deadline into a failure that names the
/// library, and the limit when one is configured.
pub(crate) fn session_helper_load_failure(
    log_file: &Path,
    memory_limit: Option<u64>,
) -> Option<String> {
    let tail = process::read_log_tail(log_file, 20).ok()?;
    let detail = tail.lines().find(|line| {
        line.contains("error while loading shared libraries")
            || line.contains("failed to map segment from shared object")
    })?;
    let mut message = format!("workspace runtime helper failed to load: {}", detail.trim());
    match memory_limit {
        Some(bytes) => message.push_str(&format!(
            ". The helper is dynamically linked, so this is either a missing or incompatible host library or a workspace memory limit too small to map it (memory_mb = {}); raise the limit and try again",
            bytes / (1024 * 1024)
        )),
        None => message.push_str(
            ". The helper is dynamically linked, so the host is missing a shared library it needs, or one is incompatible with the version it was built against",
        ),
    }
    Some(message)
}
