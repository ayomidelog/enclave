//! Running the launcher and waiting for the session it starts.
//!
//! The launcher is started with `setsid -f`, which exits as soon as it has
//! forked, so its exit status says nothing about whether the session came up. The
//! session reports that itself, through the pid file and the ready file it writes
//! from inside its own namespaces, and this module is the wait for those files and
//! the read-back of the identity behind them.
//!
//! The one failure that is retried is the kernel refusing to exec a helper whose
//! inode still has a writer, which a fork in another thread can produce for as long
//! as that fork takes to exec. Every other failure ends the attempt, and the caller
//! reaps whatever the attempt left behind.

use super::super::*;

use super::readiness::wait_for_session_ready;
use super::{log_reports_text_file_busy, LAUNCH_ATTEMPTS};

/// Launch the session, wait for its readiness files, and read back its identity.
///
/// The retry is the only failure that is repeated: the kernel refuses an exec of a
/// helper whose inode still has a writer, which a fork in another thread can
/// produce for as long as that fork takes to exec. Every other failure ends the
/// attempt, and the caller reaps whatever the attempt left behind.
pub(super) fn launch_until_ready(
    workspace: &WorkspaceMetadata,
    command: &mut Command,
    log_file: &Path,
    pid_file: &Path,
    ready_file: &Path,
    stdout: &std::fs::File,
    stderr: &std::fs::File,
) -> Result<SessionInfo> {
    let mut attempt = 0;
    loop {
        let launch = crate::perf::Timer::new("session.launch");
        let status = command
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout.try_clone().with_context(|| {
                format!("failed to clone {}", log_file.display())
            })?))
            .stderr(Stdio::from(stderr.try_clone().with_context(|| {
                format!("failed to clone {}", log_file.display())
            })?))
            .status()
            .context("failed to launch workspace session via setsid/unshare")?;
        drop(launch);

        if !status.success() {
            bail!("failed to launch workspace session (status {status})");
        }

        let ready = crate::perf::Timer::new("session.ready");
        let outcome = wait_for_session_ready(
            ready_file,
            pid_file,
            log_file,
            workspace.limits.memory_bytes,
            crate::deadlines::session_ready(),
        );
        drop(ready);
        match outcome {
            Ok(()) => break,
            Err(_error) if attempt < LAUNCH_ATTEMPTS && log_reports_text_file_busy(log_file) => {
                attempt += 1;
                let delay = Duration::from_millis(100 * attempt as u64);
                crate::perf::record_cleanup_retry();
                crate::perf::record_cleanup_retry_delay(delay.as_micros() as u64);
                tracing::debug!(
                    "retrying the workspace session launch for {} after a busy helper binary (attempt {}/{})",
                    workspace.id,
                    attempt,
                    LAUNCH_ATTEMPTS
                );
                thread::sleep(delay);
            }
            Err(error) => return Err(error),
        }
    }

    let pid = process::read_pid_file(pid_file)?;
    if !process_alive(pid) {
        let tail = process::read_log_tail(log_file, 20).unwrap_or_default();
        let rendered_tail = if tail.trim().is_empty() {
            "<empty>".to_string()
        } else {
            tail
        };
        bail!(
            "workspace session pid {} exited before startup completed. log file: {}. recent log:\n{}",
            pid,
            log_file.display(),
            rendered_tail
        );
    }

    let starttime_ticks = process_starttime_ticks(pid)?;
    let (mount_ns, pid_ns) = read_namespace_refs(pid)?;
    Ok(SessionInfo {
        pid,
        starttime_ticks,
        mount_ns,
        pid_ns,
    })
}
