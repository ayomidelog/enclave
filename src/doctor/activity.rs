//! Workspaces that look idle or are holding more work than their policy allows.

use std::path::Path;

use chrono::Utc;

use super::DoctorCheck;

/// Report idle workspaces and workspaces whose task count exceeds the configured
/// threshold. Detached work is reported separately so a busy background job is
/// not mistaken for an idle runtime.
pub(crate) fn check_workspace_activity(
    state_dir: &Path,
    default_idle_secs: Option<u64>,
    keep_running: impl Fn(&str, &str) -> bool,
    has_live_ports: impl Fn(&str, &str) -> bool,
    active_commands: impl Fn(&str, &str) -> usize,
) -> DoctorCheck {
    let name = "workspace_activity";
    let workspaces = match crate::registry::with_registry(state_dir, |registry| {
        Ok(registry
            .sandboxes
            .values()
            .flat_map(|sandbox| sandbox.workspaces.values().cloned())
            .collect::<Vec<_>>())
    }) {
        Ok(workspaces) => workspaces,
        Err(error) => {
            return DoctorCheck::warn(name, &format!("failed to read registry: {error:#}"))
        }
    };
    let now = Utc::now();
    let mut findings = Vec::new();
    for workspace in workspaces {
        if !workspace.status.is_running() {
            continue;
        }
        let Some(idle_after) = crate::workspace::idle_threshold(&workspace, default_idle_secs)
        else {
            continue;
        };
        if keep_running(&workspace.sandbox_id, &workspace.id)
            || has_live_ports(&workspace.sandbox_id, &workspace.id)
        {
            continue;
        }
        if idle_after > 0
            && crate::workspace::is_idle(
                &workspace,
                idle_after,
                now,
                active_commands(&workspace.sandbox_id, &workspace.id),
            )
        {
            findings.push(format!(
                "{} in {} is idle for at least {}s",
                workspace.id, workspace.sandbox_id, idle_after
            ));
        }
    }
    if findings.is_empty() {
        DoctorCheck::ok(name, "no workspace exceeds its idle threshold")
    } else {
        DoctorCheck::warn(name, &findings.join("; "))
    }
}

/// Report running workspaces whose task count exceeds the provided threshold.
pub(crate) fn check_task_counts(state_dir: &Path, threshold: u64) -> DoctorCheck {
    check_task_counts_with(state_dir, threshold, task_count_from_cgroup)
}

fn task_count_from_cgroup(pid: u32) -> Option<u64> {
    crate::sandbox::cgroup::runtime_cgroup_path(pid)
        .ok()
        .flatten()
        .and_then(|path| crate::sandbox::cgroup::read_cgroup_stats(&path).ok())
        .and_then(|stats| stats.pids_current)
}

pub(crate) fn check_task_counts_with(
    state_dir: &Path,
    threshold: u64,
    task_count: impl Fn(u32) -> Option<u64>,
) -> DoctorCheck {
    let name = "workspace_tasks";
    let workspaces = match crate::registry::with_registry(state_dir, |registry| {
        Ok(registry
            .sandboxes
            .values()
            .flat_map(|sandbox| sandbox.workspaces.values().cloned())
            .filter(|workspace| workspace.status.is_running())
            .collect::<Vec<_>>())
    }) {
        Ok(workspaces) => workspaces,
        Err(error) => {
            return DoctorCheck::warn(name, &format!("failed to read registry: {error:#}"))
        }
    };
    let mut findings = Vec::new();
    for workspace in workspaces {
        let Some(pid) = workspace.runtime_pid else {
            continue;
        };
        let tasks = task_count(pid);
        if let Some(tasks) = tasks.filter(|tasks| *tasks > threshold) {
            findings.push(format!(
                "{} in {} has {} tasks (threshold {})",
                workspace.id, workspace.sandbox_id, tasks, threshold
            ));
        }
    }
    if findings.is_empty() {
        DoctorCheck::ok(name, "no workspace exceeds the task-count threshold")
    } else {
        DoctorCheck::warn(name, &findings.join("; "))
    }
}

/// Report workspaces with more than one process beyond the runtime leader.
pub(crate) fn check_detached_processes(
    state_dir: &Path,
    threshold: u64,
    job_processes: impl Fn(&str, &str) -> std::collections::BTreeMap<u32, String>,
) -> DoctorCheck {
    check_detached_processes_with(state_dir, threshold, |workspace| {
        let jobs = job_processes(&workspace.sandbox_id, &workspace.id);
        workspace
            .runtime_pid
            .and_then(|pid| crate::workspace::job_process_count(pid, &jobs).ok())
            .unwrap_or(0)
    })
}

pub(crate) fn check_detached_processes_with(
    state_dir: &Path,
    threshold: u64,
    detached_count: impl Fn(&crate::workspace::WorkspaceMetadata) -> usize,
) -> DoctorCheck {
    let name = "workspace_detached_processes";
    let workspaces = match crate::registry::with_registry(state_dir, |registry| {
        Ok(registry
            .sandboxes
            .values()
            .flat_map(|sandbox| sandbox.workspaces.values().cloned())
            .filter(|workspace| workspace.status.is_running())
            .collect::<Vec<_>>())
    }) {
        Ok(workspaces) => workspaces,
        Err(error) => {
            return DoctorCheck::warn(name, &format!("failed to read registry: {error:#}"))
        }
    };
    let mut findings = Vec::new();
    for workspace in workspaces {
        if workspace.runtime_pid.is_none() {
            continue;
        }
        let detached = detached_count(&workspace);
        if detached as u64 > threshold {
            findings.push(format!(
                "{} in {} has {} detached process(es) (threshold {})",
                workspace.id, workspace.sandbox_id, detached, threshold
            ));
        }
    }
    if findings.is_empty() {
        DoctorCheck::ok(name, "no workspace exceeds the detached-process threshold")
    } else {
        DoctorCheck::warn(name, &findings.join("; "))
    }
}
