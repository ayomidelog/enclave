use super::*;

/// How often the settle wait re-reads the command line of a starting runtime.
const STARTING_POLL_INTERVAL: Duration = Duration::from_millis(10);

pub fn stop_session(pid: u32, expected_starttime_ticks: Option<u64>) -> Result<()> {
    let result = stop_sessions_batch(&[(pid, expected_starttime_ticks)])?;
    if result.failed_pids.contains(&pid) {
        return Err(crate::error::coded(
            crate::error::ErrorCode::Timeout,
            format!(
                "workspace session pid {} did not exit after SIGTERM and SIGKILL within {}",
                pid,
                crate::deadlines::runtime_kill_grace().describe_timeout()
            ),
        ));
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
    let mut sorted = StopTargets::sort(targets)?;

    // A runtime that has been forked but has not finished the exec that makes it
    // the runtime still carries its launcher's command line, and a process inside
    // that exec reads as having none at all. Reading that moment as a stale record
    // is what clears the record and leaves the runtime holding its cgroup, its
    // interface, and its mounts with nothing left to name it, so the command line
    // gets the settle deadline to arrive before the record is given up on. The
    // deadline bounds the whole batch rather than each record, so a stop that finds
    // several unrecognizable pids pays it once.
    sorted.settle(crate::deadlines::runtime_exec_settle().get())?;
    for (pid, expected_starttime_ticks) in std::mem::take(&mut sorted.starting) {
        tracing::warn!(
            "session pid {pid} is still not an enclave runtime after {}; treating its record as stale",
            crate::deadlines::runtime_exec_settle().describe_timeout()
        );
        mark_already_stopped(&mut result, pid, expected_starttime_ticks);
    }
    for (pid, expected_starttime_ticks) in std::mem::take(&mut sorted.stopped) {
        mark_already_stopped(&mut result, pid, expected_starttime_ticks);
    }
    let pending = sorted.signallable;

    for (pid, _) in &pending {
        process::send_signal(*pid, libc::SIGTERM)?;
    }
    after_signal();
    // Dedicated cgroups provide a fast fallback for the remaining process tree,
    // so the graceful window is short: every workspace would otherwise pay a
    // fixed multi-second delay before cgroup.kill is used.
    wait_for_targets_to_exit(&pending, crate::deadlines::runtime_term_grace().get());

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
    wait_for_targets_to_exit(&remaining, crate::deadlines::runtime_kill_grace().get());

    // Some kernels report a successful cgroup.kill write before the leader
    // disappears. Keep the PID/start-time guard and issue one direct fallback
    // signal so a surviving runtime cannot make the whole sandbox stop fail.
    let still_running = collect_running_targets(&pending);
    for (pid, expected_starttime_ticks) in &still_running {
        let signallable = process::verify_signal_target(*pid, *expected_starttime_ticks)
            .is_ok_and(|target| target.is_signallable());
        if signallable {
            process::send_signal(*pid, libc::SIGKILL)?;
        }
    }
    wait_for_targets_to_exit(&still_running, crate::deadlines::runtime_kill_grace().get());

    for (pid, expected_starttime_ticks) in pending {
        if process_matches(pid, expected_starttime_ticks) {
            result.failed_pids.insert(pid);
        } else {
            mark_already_stopped(&mut result, pid, expected_starttime_ticks);
        }
    }

    remaining.clear();
    Ok(result)
}

/// Record a pid the stop will not signal as already stopped.
///
/// The cached namespace descriptors and the persistent helper of a pid that is
/// gone describe nothing, so they are dropped with the record. A pid whose
/// command line never named a runtime has no helper, and invalidating is a no-op.
fn mark_already_stopped(
    result: &mut BatchStopResult,
    pid: u32,
    expected_starttime_ticks: Option<u64>,
) {
    result.stopped_pids.insert(pid);
    namespace_cache::invalidate(pid, expected_starttime_ticks);
    if let Some(starttime_ticks) = expected_starttime_ticks {
        persistent::invalidate(pid, starttime_ticks);
    }
}

/// The targets of a stop, sorted by what the stop does with each one.
///
/// Sorting is separate from acting because a target can change state between the
/// two: a runtime that is still inside the exec that makes it the runtime when it
/// is first inspected becomes recognizable a moment later, which is why the
/// starting bucket is re-examined under a deadline instead of being given up on
/// the first time it is seen.
#[derive(Default)]
struct StopTargets {
    /// Pids to signal: alive, and recognizable as runtimes this daemon owns.
    signallable: Vec<(u32, Option<u64>)>,
    /// Pids that are alive and match their record, but whose command line does not
    /// name a runtime yet.
    starting: Vec<(u32, Option<u64>)>,
    /// Pids whose record is stale: the process is gone, or it belongs to another
    /// user.
    stopped: Vec<(u32, Option<u64>)>,
}

impl StopTargets {
    /// Sort `targets` by what the stop does with each one.
    fn sort(targets: &[(u32, Option<u64>)]) -> Result<Self> {
        let mut sorted = Self::default();
        for (pid, expected_starttime_ticks) in targets.iter().copied() {
            if !process_matches(pid, expected_starttime_ticks) {
                sorted.stopped.push((pid, expected_starttime_ticks));
                continue;
            }
            match process::verify_signal_target(pid, expected_starttime_ticks)? {
                process::SignalTarget::Signallable => {
                    sorted.signallable.push((pid, expected_starttime_ticks));
                }
                process::SignalTarget::Starting => {
                    sorted.starting.push((pid, expected_starttime_ticks));
                }
                process::SignalTarget::ForeignOwner { owner_uid } => {
                    tracing::warn!(
                        "stale session pid {pid} detected (owned by uid {owner_uid}); treating as already stopped"
                    );
                    sorted.stopped.push((pid, expected_starttime_ticks));
                }
            }
        }
        Ok(sorted)
    }

    /// Re-inspect the targets whose command line had not settled, for up to
    /// `timeout`.
    ///
    /// Whatever becomes recognizable joins the signallable bucket and whatever
    /// exits joins the stopped bucket. A target that is still unrecognizable when
    /// the deadline passes stays in the starting bucket, for the caller to report.
    fn settle(&mut self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        while !self.starting.is_empty() {
            if Instant::now() >= deadline {
                return Ok(());
            }
            thread::sleep(STARTING_POLL_INTERVAL);
            let waiting = std::mem::take(&mut self.starting);
            let settled = Self::sort(&waiting)?;
            self.signallable.extend(settled.signallable);
            self.starting.extend(settled.starting);
            self.stopped.extend(settled.stopped);
        }
        Ok(())
    }
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
