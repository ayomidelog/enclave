//! The live half of workspace activity.
//!
//! Persisted timestamps answer when a workspace was last touched. They cannot
//! answer whether a foreground command is still running: a long command updates
//! its timestamp once and then stays busy. This service owns that second answer.
//!
//! A command lease is acquired before a request reaches the workspace and held
//! until the request returns. A background job does not hold a lease; it pins
//! its workspace only when it was started with an explicit `keep_running`
//! policy. The idle sweeper checks both, so it cannot stop a workspace out from
//! under an active caller while still allowing ordinary detached work to age out.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::Result;
use chrono::{DateTime, Utc};

use crate::registry::{with_registry, with_registry_mut};
use crate::workspace::jobs::{self, JobMode, JobRecord, JobStatus};
use crate::workspace::session;
use crate::workspace::types::{WorkspaceMetadata, WorkspaceStatus};
use crate::workspace::{activity, processes};

/// A lease for one active request.
pub struct CommandLease {
    service: Arc<ActivityService>,
    key: String,
}

impl Drop for CommandLease {
    fn drop(&mut self) {
        self.service.release(&self.key);
    }
}

/// The daemon-owned view of what is active and what is idle.
pub struct ActivityService {
    leases: Mutex<BTreeMap<String, usize>>,
    external: Mutex<BTreeMap<String, ExternalLease>>,
    jobs: Mutex<BTreeMap<String, JobRecord>>,
}

impl Default for ActivityService {
    fn default() -> Self {
        Self::new()
    }
}

impl ActivityService {
    pub fn new() -> Self {
        Self {
            leases: Mutex::new(BTreeMap::new()),
            external: Mutex::new(BTreeMap::new()),
            jobs: Mutex::new(BTreeMap::new()),
        }
    }

    /// Load jobs that are still alive after startup reconciliation.
    ///
    /// This is the startup path: dead records are reconciled on disk first, and
    /// only live ones need a place in the daemon's map. Terminal records remain
    /// available through `jobs::list` for `workspace jobs`.
    pub fn load_live_jobs(&self, state_dir: &Path) -> Result<()> {
        let mut live = BTreeMap::new();
        for job in jobs::list(state_dir)? {
            if job.status == JobStatus::Running && job_alive(&job) {
                live.insert(job.id.clone(), job);
            }
        }
        *self.lock_jobs() = live;
        Ok(())
    }

    /// Take a lease for a request and touch its activity in one step.
    ///
    /// The request handlers use this so a direct command's timestamp and its
    /// lease are established before the command can race an idle sweep.
    pub fn begin_request(
        self: &Arc<Self>,
        state_dir: &Path,
        sandbox_id: &str,
        workspace_id: &str,
    ) -> Result<CommandLease> {
        let lease = self.acquire(sandbox_id, workspace_id);
        activity::touch_registry_only(
            state_dir,
            sandbox_id,
            workspace_id,
            activity::ActivityKind::Request,
        )?;
        Ok(lease)
    }

    /// Take a lease for a request that is about to act on a workspace.
    pub fn acquire(self: &Arc<Self>, sandbox_id: &str, workspace_id: &str) -> CommandLease {
        let key = workspace_key(sandbox_id, workspace_id);
        let mut leases = self.lock_leases();
        *leases.entry(key.clone()).or_insert(0) += 1;
        drop(leases);
        CommandLease {
            service: Arc::clone(self),
            key,
        }
    }

    fn release(&self, key: &str) {
        let mut leases = self.lock_leases();
        if let Some(count) = leases.get_mut(key) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                leases.remove(key);
            }
        }
    }

    /// Take a lease on behalf of a CLI process that will run the command
    /// directly. The lease remains until the CLI releases it or its owner exits.
    pub fn begin_external(
        &self,
        sandbox_id: &str,
        workspace_id: &str,
        owner_pid: u32,
        owner_starttime_ticks: u64,
    ) -> String {
        let token = uuid::Uuid::new_v4().to_string();
        self.lock_external().insert(
            token.clone(),
            ExternalLease {
                sandbox_id: sandbox_id.to_string(),
                workspace_id: workspace_id.to_string(),
                owner_pid,
                owner_starttime_ticks,
            },
        );
        token
    }

    pub fn end_external(&self, token: &str) -> bool {
        self.lock_external().remove(token).is_some()
    }

    /// How many live command leases a workspace holds.
    pub fn active_commands(&self, sandbox_id: &str, workspace_id: &str) -> usize {
        let live = self
            .lock_leases()
            .get(&workspace_key(sandbox_id, workspace_id))
            .copied()
            .unwrap_or(0);
        let mut external = self.lock_external();
        // A CLI that exits without releasing its token is not a lease anymore.
        // Reaping the entry here keeps a long-lived daemon from accumulating
        // dead owners and makes the count the truth at the moment it is asked.
        external.retain(|_, lease| {
            session::process_matches(lease.owner_pid, Some(lease.owner_starttime_ticks))
        });
        let external = external
            .values()
            .filter(|lease| lease.sandbox_id == sandbox_id && lease.workspace_id == workspace_id)
            .count();
        live.saturating_add(external)
    }

    /// Whether a foreground job from a detached helper or a previous daemon is
    /// still alive. The in-memory lease covers only this daemon's own request.
    pub fn has_active_foreground_job(&self, sandbox_id: &str, workspace_id: &str) -> bool {
        self.jobs_for_workspace(sandbox_id, workspace_id)
            .into_iter()
            .any(|job| {
                job.mode == JobMode::Foreground
                    && job.status == JobStatus::Running
                    && job_alive(&job)
            })
    }

    /// Remove terminal job records from the live map.
    ///
    /// The durable files stay for `workspace jobs`; this only prevents a stopped
    /// workspace's old running entries from pinning its next runtime.
    pub fn prune_terminal_jobs(&self, sandbox_id: &str, workspace_id: &str) {
        self.lock_jobs().retain(|_, job| {
            job.sandbox_id != sandbox_id
                || job.workspace_id != workspace_id
                || job.status == JobStatus::Running
        });
    }

    /// Promote durable jobs that are still alive into the live map.
    ///
    /// The daemon may have restarted since a job started. Startup reconciliation
    /// leaves live records on disk; this recovers them before a sweep decides the
    /// workspace is idle.
    pub fn refresh_jobs_for_workspace(
        &self,
        state_dir: &Path,
        sandbox_id: &str,
        workspace_id: &str,
    ) -> Result<()> {
        for job in jobs::list_for_workspace(state_dir, sandbox_id, workspace_id)? {
            if job.status == JobStatus::Running && job_alive(&job) {
                self.update_job(job);
            }
        }
        Ok(())
    }

    /// Remember a job for the lifetime of the daemon. The durable copy is written
    /// by the caller after its process identity is known.
    pub fn register_job(&self, job: JobRecord) {
        self.lock_jobs().insert(job.id.clone(), job);
    }

    pub fn update_job(&self, job: JobRecord) {
        self.lock_jobs().insert(job.id.clone(), job);
    }

    pub fn finish_job(
        &self,
        state_dir: &Path,
        job_id: &str,
        status: JobStatus,
        exit_code: Option<i32>,
        error: Option<String>,
    ) -> Result<bool> {
        let changed = jobs::finish_if_running(state_dir, job_id, status, exit_code, error.clone())?;
        if !changed {
            return Ok(false);
        }
        if let Some(job) = self.lock_jobs().get_mut(job_id) {
            job.status = status;
            job.exit_code = exit_code;
            job.finished_at = Some(activity::now());
            job.error = error;
        }
        Ok(true)
    }

    pub fn job(&self, job_id: &str) -> Option<JobRecord> {
        self.lock_jobs().get(job_id).cloned()
    }

    /// The live jobs for one workspace.
    pub fn jobs_for_workspace(&self, sandbox_id: &str, workspace_id: &str) -> Vec<JobRecord> {
        self.lock_jobs()
            .values()
            .filter(|job| job.sandbox_id == sandbox_id && job.workspace_id == workspace_id)
            .cloned()
            .collect()
    }

    /// Whether a background job explicitly asks to keep its workspace running.
    pub fn keeps_workspace_running(&self, sandbox_id: &str, workspace_id: &str) -> bool {
        self.jobs_for_workspace(sandbox_id, workspace_id)
            .into_iter()
            .any(|job| {
                job.mode == JobMode::Background
                    && job.keep_running
                    && job.status == JobStatus::Running
            })
    }

    /// The process groups a workspace currently owns through job records.
    pub fn job_processes(&self, sandbox_id: &str, workspace_id: &str) -> BTreeMap<u32, String> {
        self.jobs_for_workspace(sandbox_id, workspace_id)
            .into_iter()
            .filter(|job| job.mode == JobMode::Background && job.status == JobStatus::Running)
            .filter_map(|job| {
                // A detached helper is made the leader of its own process
                // group before exec, so the helper pid is also the group id
                // inherited by every process the command starts.
                job.process_group
                    .and_then(|pgid| u32::try_from(pgid).ok())
                    .map(|pgid| (pgid, job.id.clone()))
            })
            .collect()
    }

    /// Stop background jobs whose explicit TTL has expired.
    pub fn reap_expired_jobs(
        &self,
        state_dir: &Path,
        now: DateTime<Utc>,
    ) -> Result<Vec<(String, String)>> {
        // Recover live records left by a previous daemon before deciding which
        // TTLs have expired. The durable list is authoritative here, so a job
        // cannot escape its TTL merely because the live map was rebuilt.
        let expired = jobs::list(state_dir)?
            .into_iter()
            .filter(|job| job.status == JobStatus::Running && job.mode == JobMode::Background)
            .filter(|job| job.expired(now))
            .collect::<Vec<_>>();
        let mut cancelled = Vec::new();
        for mut job in expired {
            if !job_alive(&job) {
                job.status = JobStatus::Failed;
                job.finished_at = Some(activity::now());
                job.error = Some("background job exited before its TTL expired".to_string());
                jobs::persist(state_dir, &job)?;
                self.update_job(job.clone());
                continue;
            }
            let exited = terminate_job(&job);
            job.status = if exited {
                JobStatus::Cancelled
            } else {
                JobStatus::Failed
            };
            job.finished_at = Some(activity::now());
            job.error = Some(if exited {
                "background job TTL expired".to_string()
            } else {
                "background job TTL expired but its process group survived SIGKILL".to_string()
            });
            jobs::persist(state_dir, &job)?;
            self.update_job(job.clone());
            if exited {
                cancelled.push((job.workspace_id.clone(), job.id.clone()));
            }
        }
        Ok(cancelled)
    }

    fn lock_leases(&self) -> MutexGuard<'_, BTreeMap<String, usize>> {
        self.leases
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    fn lock_jobs(&self) -> MutexGuard<'_, BTreeMap<String, JobRecord>> {
        self.jobs.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn lock_external(&self) -> MutexGuard<'_, BTreeMap<String, ExternalLease>> {
        self.external
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }
}

struct ExternalLease {
    sandbox_id: String,
    workspace_id: String,
    owner_pid: u32,
    owner_starttime_ticks: u64,
}

/// A workspace's identity for the live lease map.
pub fn workspace_key(sandbox_id: &str, workspace_id: &str) -> String {
    format!("{sandbox_id}/{workspace_id}")
}

/// Persist a job update and keep the daemon copy in step.
pub fn persist_job(state_dir: &std::path::Path, job: &JobRecord) -> Result<()> {
    jobs::persist(state_dir, job)
}

/// The process-group map for one workspace, read from durable jobs.
///
/// `ps` is a daemon request but the process table is assembled from the state
/// directory, so it uses the same records as the live service without requiring
/// a second copy of them in a read-only worker.
pub fn job_processes_from_disk(
    state_dir: &Path,
    sandbox_id: &str,
    workspace_id: &str,
) -> Result<BTreeMap<u32, String>> {
    Ok(
        jobs::list_for_workspace(state_dir, sandbox_id, workspace_id)?
            .into_iter()
            .filter(|job| job.mode == JobMode::Background && job.status == JobStatus::Running)
            .filter_map(|job| {
                job.process_group
                    .and_then(|pgid| u32::try_from(pgid).ok())
                    .map(|pgid| (pgid, job.id.clone()))
            })
            .collect(),
    )
}

/// The workspaces eligible for an idle stop, in least-recently-used order.
pub fn idle_candidates(
    state_dir: &std::path::Path,
    service: &ActivityService,
    default_idle_secs: Option<u64>,
    now: DateTime<Utc>,
) -> Result<Vec<WorkspaceMetadata>> {
    let workspaces = with_registry(state_dir, |registry| {
        Ok(registry
            .sandboxes
            .values()
            .flat_map(|sandbox| sandbox.workspaces.values().cloned())
            .collect::<Vec<_>>())
    })?;
    let mut candidates = Vec::new();
    for workspace in workspaces {
        let Some(idle_after) = activity::idle_threshold(&workspace, default_idle_secs) else {
            continue;
        };
        if service.keeps_workspace_running(&workspace.sandbox_id, &workspace.id)
            || service.has_active_foreground_job(&workspace.sandbox_id, &workspace.id)
        {
            continue;
        }
        if activity::is_idle(
            &workspace,
            idle_after,
            now,
            service.active_commands(&workspace.sandbox_id, &workspace.id),
        ) {
            candidates.push(workspace);
        }
    }
    candidates.sort_by(|a, b| {
        activity::last_activity(a)
            .cmp(&activity::last_activity(b))
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(candidates)
}

/// The running workspaces in one sandbox, for capacity checks.
pub fn running_workspaces(
    state_dir: &std::path::Path,
    sandbox_id: Option<&str>,
) -> Result<Vec<WorkspaceMetadata>> {
    with_registry(state_dir, |registry| {
        Ok(registry
            .sandboxes
            .values()
            .filter(|sandbox| sandbox_id.is_none_or(|id| sandbox.metadata.id == id))
            .flat_map(|sandbox| sandbox.workspaces.values())
            .filter(|workspace| workspace.status == WorkspaceStatus::Running)
            .cloned()
            .collect())
    })
}

/// The workspaces that are stopped and old enough for destroy-after.
pub fn destroy_candidates(
    state_dir: &std::path::Path,
    default_destroy_secs: Option<u64>,
    now: DateTime<Utc>,
) -> Result<Vec<WorkspaceMetadata>> {
    let workspaces = with_registry(state_dir, |registry| {
        Ok(registry
            .sandboxes
            .values()
            .flat_map(|sandbox| sandbox.workspaces.values().cloned())
            .collect::<Vec<_>>())
    })?;
    Ok(workspaces
        .into_iter()
        .filter(|workspace| workspace.status == WorkspaceStatus::Stopped && !workspace.keep_running)
        .filter(|workspace| {
            let Some(threshold) = activity::destroy_threshold(workspace, default_destroy_secs)
            else {
                return false;
            };
            if threshold == 0 {
                return false;
            }
            workspace
                .last_stopped_at
                .as_deref()
                .and_then(activity::parse_timestamp)
                .is_some_and(|stopped| (now - stopped).num_seconds() >= threshold as i64)
        })
        .collect())
}

/// Mark a persisted job as failed after its supervisor disappeared.
pub fn reconcile_jobs(state_dir: &Path) -> Result<usize> {
    let mut changed = 0usize;
    for mut job in jobs::list(state_dir)? {
        if job.status != JobStatus::Running {
            continue;
        }
        let alive = job_alive(&job);
        if alive {
            continue;
        }
        job.status = JobStatus::Failed;
        job.finished_at = Some(activity::now());
        job.error = Some("the daemon restarted while this job was running".to_string());
        jobs::persist(state_dir, &job)?;
        changed += 1;
    }
    Ok(changed)
}

/// Reconcile jobs owned by one workspace against their live process identities.
pub fn reconcile_workspace_jobs(
    state_dir: &Path,
    service: &ActivityService,
    sandbox_id: &str,
    workspace_id: &str,
) -> Result<usize> {
    let mut changed = 0usize;
    for mut job in jobs::list_for_workspace(state_dir, sandbox_id, workspace_id)? {
        if job.status != JobStatus::Running {
            continue;
        }
        if job_alive(&job) {
            service.update_job(job);
            continue;
        }
        job.status = JobStatus::Failed;
        job.finished_at = Some(activity::now());
        job.error = Some("job process exited before its supervisor observed it".to_string());
        jobs::persist(state_dir, &job)?;
        service.update_job(job);
        changed += 1;
    }
    Ok(changed)
}

/// Whether a job still has the process identity it recorded.
pub fn job_alive(job: &JobRecord) -> bool {
    let Some(pid) = job.pid else {
        return false;
    };
    if processes::process_matches(pid, job.starttime_ticks) {
        return true;
    }
    job.process_group.is_some_and(|pgid| {
        processes::process_group_alive(pgid, job.starttime_ticks)
            || processes::process_group_has_members(pgid, job.starttime_ticks)
    })
}

/// Terminate a TTL-expired job, escalating to SIGKILL after the configured
/// graceful window. A process that ignores SIGTERM must not keep its job record
/// and its workspace resources alive forever.
fn terminate_job(job: &JobRecord) -> bool {
    if !job_alive(job) {
        return true;
    }
    signal_job(job, libc::SIGTERM);
    if wait_for_job_exit(job, crate::deadlines::runtime_term_grace().get()) {
        return true;
    }
    signal_job(job, libc::SIGKILL);
    wait_for_job_exit(job, crate::deadlines::runtime_kill_grace().get())
}

fn signal_job(job: &JobRecord, signal: i32) {
    let Some(pid) = job.pid else {
        return;
    };
    if let Some(pgid) = job.process_group {
        if session::signal_process_group(pid, job.starttime_ticks, pgid, signal).is_ok() {
            return;
        }
    }
    if processes::process_matches(pid, job.starttime_ticks) {
        let _ = unsafe { libc::kill(pid as i32, signal) };
    }
}

fn wait_for_job_exit(job: &JobRecord, timeout: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if !job_alive(job) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    !job_alive(job)
}

/// Mark stopped workspaces' timestamps in a registry snapshot.
pub fn mark_stopped_workspaces(state_dir: &Path, workspace_ids: &BTreeSet<String>) -> Result<()> {
    if workspace_ids.is_empty() {
        return Ok(());
    }
    with_registry_mut(state_dir, |registry| {
        for sandbox in registry.sandboxes.values_mut() {
            for workspace in sandbox.workspaces.values_mut() {
                if workspace_ids.contains(&workspace.id) {
                    activity::mark_stopped(workspace);
                    crate::workspace::control::persist_workspace_metadata(workspace)?;
                }
            }
        }
        Ok(())
    })
}
