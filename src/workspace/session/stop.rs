use super::*;

pub fn stop_session(pid: u32, expected_starttime_ticks: Option<u64>) -> Result<()> {
    let result = stop_sessions_batch(&[(pid, expected_starttime_ticks)])?;
    if result.failed_pids.contains(&pid) {
        bail!("workspace session pid {} did not exit after SIGKILL", pid);
    }
    Ok(())
}

pub fn stop_sessions_batch(targets: &[(u32, Option<u64>)]) -> Result<BatchStopResult> {
    stop_sessions_batch_with_hook(targets, || {})
}

/// Stop runtimes while allowing independent cleanup to begin immediately
/// after SIGTERM is sent and before the shared graceful-exit wait.
pub fn stop_sessions_batch_with_hook<F>(
    targets: &[(u32, Option<u64>)],
    after_signal: F,
) -> Result<BatchStopResult>
where
    F: FnOnce(),
{
    let mut result = BatchStopResult::default();
    let mut pending = Vec::new();

    for (pid, expected_starttime_ticks) in targets.iter().copied() {
        if !process_matches(pid, expected_starttime_ticks) {
            result.stopped_pids.insert(pid);
            namespace_cache::invalidate(pid, expected_starttime_ticks);
            if let Some(starttime_ticks) = expected_starttime_ticks {
                persistent::invalidate(pid, starttime_ticks);
            }
            continue;
        }
        match process::verify_signal_target(pid, expected_starttime_ticks) {
            Ok(()) => pending.push((pid, expected_starttime_ticks)),
            Err(err) => {
                let msg = format!("{err:#}");
                if msg.contains("refusing to signal") || msg.contains("does not look like") {
                    tracing::warn!(
                        "stale session pid {} detected (not an enclave process); treating as already stopped",
                        pid
                    );
                    result.stopped_pids.insert(pid);
                    namespace_cache::invalidate(pid, expected_starttime_ticks);
                    if let Some(starttime_ticks) = expected_starttime_ticks {
                        persistent::invalidate(pid, starttime_ticks);
                    }
                    continue;
                }
                return Err(err);
            }
        }
    }

    for (pid, _) in &pending {
        process::send_signal(*pid, libc::SIGTERM)?;
    }
    after_signal();
    wait_for_targets_to_exit(&pending, STOP_TIMEOUT);

    let mut remaining = collect_running_targets(&pending);
    for (pid, _) in &remaining {
        let cgroup_killed = crate::sandbox::cgroup::runtime_cgroup_path(*pid)?
            .filter(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("enclave-ws-"))
            })
            .filter(|path| crate::sandbox::cgroup::cgroup_contains_pid(path, *pid).unwrap_or(false))
            .map(|path| crate::sandbox::cgroup::kill_cgroup_members(&path))
            .transpose()?
            .unwrap_or(false);
        if !cgroup_killed {
            process::send_signal(*pid, libc::SIGKILL)?;
        }
    }
    wait_for_targets_to_exit(&remaining, POST_KILL_TIMEOUT);

    // Some kernels report a successful cgroup.kill write before the leader
    // disappears. Keep the PID/start-time guard and issue one direct fallback
    // signal so a surviving runtime cannot make the whole sandbox stop fail.
    let still_running = collect_running_targets(&pending);
    for (pid, expected_starttime_ticks) in &still_running {
        if process::verify_signal_target(*pid, *expected_starttime_ticks).is_ok() {
            process::send_signal(*pid, libc::SIGKILL)?;
        }
    }
    wait_for_targets_to_exit(&still_running, POST_KILL_TIMEOUT);

    for (pid, expected_starttime_ticks) in pending {
        if process_matches(pid, expected_starttime_ticks) {
            result.failed_pids.insert(pid);
        } else {
            result.stopped_pids.insert(pid);
            namespace_cache::invalidate(pid, expected_starttime_ticks);
            if let Some(starttime_ticks) = expected_starttime_ticks {
                persistent::invalidate(pid, starttime_ticks);
            }
        }
    }

    remaining.clear();
    Ok(result)
}

pub(crate) fn wait_for_targets_to_exit(targets: &[(u32, Option<u64>)], timeout: Duration) {
    let pidfds = targets
        .iter()
        .filter_map(|(pid, expected)| {
            if !process_matches(*pid, *expected) {
                return None;
            }
            open_pidfd(*pid).map(|fd| (fd, *pid, *expected))
        })
        .collect::<Vec<_>>();
    if !pidfds.is_empty() && pidfds.len() == targets.len() {
        wait_for_pidfds(&pidfds, timeout);
        // pidfd readiness is an exit hint. Keep the start-time checks below
        // authoritative because a runtime can be replaced after a timeout.
        for (fd, _, _) in pidfds {
            unsafe { libc::close(fd) };
        }
        return;
    }
    for (fd, _, _) in pidfds {
        unsafe { libc::close(fd) };
    }

    let started = Instant::now();
    while started.elapsed() < timeout {
        if targets.iter().all(|(pid, expected_starttime_ticks)| {
            !process_matches(*pid, *expected_starttime_ticks)
        }) {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

pub(crate) fn open_pidfd(pid: u32) -> Option<i32> {
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) as i32 };
    (fd >= 0).then_some(fd)
}

pub(crate) fn wait_for_pidfds(pidfds: &[(i32, u32, Option<u64>)], timeout: Duration) {
    let deadline = Instant::now() + timeout;
    let mut descriptors = pidfds
        .iter()
        .map(|(fd, _, _)| libc::pollfd {
            fd: *fd,
            events: libc::POLLIN,
            revents: 0,
        })
        .collect::<Vec<_>>();
    while !descriptors.is_empty() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return;
        }
        let timeout_ms = remaining.as_millis().clamp(1, i32::MAX as u128) as i32;
        let result = unsafe {
            libc::poll(
                descriptors.as_mut_ptr(),
                descriptors.len() as libc::nfds_t,
                timeout_ms,
            )
        };
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            tracing::debug!("pidfd poll failed; falling back to process checks: {error}");
            return;
        }
        if result == 0 {
            return;
        }
        descriptors.retain(|descriptor| {
            descriptor.revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP) == 0
        });
    }
}

pub(crate) fn collect_running_targets(targets: &[(u32, Option<u64>)]) -> Vec<(u32, Option<u64>)> {
    targets
        .iter()
        .copied()
        .filter(|(pid, expected_starttime_ticks)| process_matches(*pid, *expected_starttime_ticks))
        .collect()
}
