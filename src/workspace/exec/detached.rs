//! Launching a long-running workspace command without holding the request open.

use std::process::Stdio;
use std::sync::Arc;
use std::thread;

use anyhow::Result;

use crate::workspace::types::WorkspaceMetadata;
use crate::workspace::ActivityService;

/// Launch a long-running command without keeping the daemon request open.
/// The helper wrapper is attached to the workspace cgroup before it forks the
/// command, so descendants follow the workspace lifecycle.
pub(crate) fn spawn_workspace_command_detached(
    state_dir: &std::path::Path,
    workspace: &WorkspaceMetadata,
    cwd: &str,
    command: &[String],
    ttl_secs: Option<u64>,
    keep_running: bool,
    activity: Arc<ActivityService>,
) -> Result<crate::workspace::JobRecord> {
    let mut job = crate::workspace::JobRecord::new(
        &workspace.sandbox_id,
        &workspace.id,
        crate::workspace::JobMode::Background,
        command,
    );
    job.ttl_secs = ttl_secs;
    job.keep_running = keep_running;
    crate::workspace::jobs::persist(state_dir, &job)?;
    activity.register_job(job.clone());
    let child = match super::spawn_workspace_command_in_group(
        workspace,
        cwd,
        command,
        Stdio::null(),
        Stdio::null(),
        Stdio::null(),
    ) {
        Ok(child) => child,
        Err(error) => {
            job.status = crate::workspace::JobStatus::Failed;
            job.error = Some(format!("{error:#}"));
            job.finished_at = Some(crate::workspace::activity_now());
            crate::workspace::jobs::persist(state_dir, &job)?;
            activity.update_job(job);
            return Err(error);
        }
    };
    let pid = child.id();
    job.pid = Some(pid);
    job.starttime_ticks = crate::workspace::session::process_starttime_ticks(pid).ok();
    // The helper is made a process-group leader before exec. Only record a
    // group identity once the kernel confirms it; a pid is not evidence that
    // `setpgid` succeeded, and signalling a group by assumption could reach a
    // group the helper never owned.
    job.process_group = crate::workspace::session::process_group_id(pid)
        .ok()
        .filter(|pgid| i32::try_from(pid).is_ok_and(|pid| *pgid == pid));
    if job.process_group.is_none() {
        // The helper is the process the daemon waits on and can signal even if
        // the kernel did not confirm it as its own group leader. The group is a
        // bonus identity for reaping descendants; the helper identity is still
        // what proves the job.
        tracing::warn!(
            job = %job.id,
            pid,
            "detached helper is not its own process-group leader; tracking its pid instead"
        );
    }
    crate::workspace::jobs::persist(state_dir, &job)?;
    activity.update_job(job.clone());
    crate::workspace::touch_activity(
        state_dir,
        &workspace.sandbox_id,
        &workspace.id,
        crate::workspace::ActivityKind::Process,
    )?;
    let state_dir = state_dir.to_path_buf();
    let job_id = job.id.clone();
    let job_process_group = job.process_group;
    let job_starttime_ticks = job.starttime_ticks;
    let activity_for_waiter = Arc::clone(&activity);
    thread::spawn(move || {
        let mut child = child;
        let outcome = child.wait();
        // A helper that exits before its descendants leaves work that no
        // process is supervising. Keep the job bounded: give the group a
        // graceful signal, then use the same kill escalation as a TTL expiry.
        if let Some(pgid) = job_process_group {
            if crate::workspace::process_group_has_members(pgid, job_starttime_ticks) {
                let _ = crate::workspace::session::signal_process_group(
                    pid,
                    job_starttime_ticks,
                    pgid,
                    libc::SIGTERM,
                );
                let deadline =
                    std::time::Instant::now() + crate::deadlines::runtime_term_grace().get();
                while crate::workspace::process_group_has_members(pgid, job_starttime_ticks)
                    && std::time::Instant::now() < deadline
                {
                    thread::sleep(std::time::Duration::from_millis(25));
                }
                if crate::workspace::process_group_has_members(pgid, job_starttime_ticks) {
                    let _ = crate::workspace::session::signal_process_group(
                        pid,
                        job_starttime_ticks,
                        pgid,
                        libc::SIGKILL,
                    );
                    let deadline =
                        std::time::Instant::now() + crate::deadlines::runtime_kill_grace().get();
                    while crate::workspace::process_group_has_members(pgid, job_starttime_ticks)
                        && std::time::Instant::now() < deadline
                    {
                        thread::sleep(std::time::Duration::from_millis(25));
                    }
                }
            }
        }
        let group_survived = job_process_group.is_some_and(|pgid| {
            crate::workspace::process_group_has_members(pgid, job_starttime_ticks)
        });
        let status = match outcome {
            Ok(status) if status.success() && !group_survived => {
                crate::workspace::JobStatus::Exited
            }
            Ok(_) => crate::workspace::JobStatus::Failed,
            Err(_) => crate::workspace::JobStatus::Failed,
        };
        let code = outcome.ok().and_then(|status| status.code());
        let error = group_survived
            .then(|| "the command exited but processes it started are still running".to_string());
        if let Err(error) = activity_for_waiter.finish_job(&state_dir, &job_id, status, code, error)
        {
            tracing::warn!("failed to persist detached job outcome for {job_id}: {error:#}");
        }
    });
    Ok(job)
}
