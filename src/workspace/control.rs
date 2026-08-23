use std::collections::{BTreeSet, VecDeque};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{anyhow, bail, Context, Result};

use crate::network;
use crate::registry::{with_registry, with_registry_mut, RegistrySandbox};
use crate::sandbox::{effective_rootfs_path, resolve_sandbox_id, SandboxMetadata, SandboxStatus};

use super::ports::{merge_published_port_statuses, PublishedPortSpec, PublishedPortStatus};
use super::session;
use super::types::{
    WorkspaceLimitsUpdate, WorkspaceListItem, WorkspaceMetadata, WorkspaceResizeResult,
    WorkspaceStatus, WorkspaceStatusReport,
};

struct WorkspaceRuntimeStart {
    pid: u32,
    starttime_ticks: u64,
    mount_ns: String,
    pid_ns: String,
    assigned_ip: String,
}

enum NetworkStartPlan {
    AllocateFromUsedIps(BTreeSet<u8>),
}

pub fn list_workspaces(
    state_dir: &std::path::Path,
    sandbox_selector: Option<&str>,
) -> Result<Vec<WorkspaceMetadata>> {
    with_registry(state_dir, |registry| {
        let mut workspaces = Vec::new();

        if let Some(selector) = sandbox_selector {
            let sandbox_id = resolve_sandbox_id(registry, selector)?;
            let sandbox = registry
                .sandboxes
                .get(&sandbox_id)
                .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_id))?;
            workspaces.extend(sandbox.workspaces.values().cloned());
        } else {
            for sandbox in registry.sandboxes.values() {
                workspaces.extend(sandbox.workspaces.values().cloned());
            }
        }

        workspaces.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(workspaces)
    })
}

pub fn remove_workspace(
    state_dir: &std::path::Path,
    sandbox_selector: &str,
    workspace_selector: &str,
) -> Result<()> {
    destroy_workspace(state_dir, sandbox_selector, workspace_selector).map(|_| ())
}

pub fn destroy_workspace(
    state_dir: &std::path::Path,
    sandbox_selector: &str,
    workspace_selector: &str,
) -> Result<String> {
    let (sandbox, workspace) = with_registry(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, sandbox_selector)?;
        let sandbox = registry
            .sandboxes
            .get(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_id))?;

        let workspace_id = resolve_workspace_id(sandbox, workspace_selector)?;
        let workspace = sandbox
            .workspaces
            .get(&workspace_id)
            .cloned()
            .ok_or_else(|| {
                anyhow!(
                    "workspace '{}' not found in sandbox '{}'",
                    workspace_id,
                    sandbox_id
                )
            })?;

        Ok((sandbox.metadata.clone(), workspace))
    })?;

    let workspace_id = workspace.id.clone();
    cleanup_workspace_artifacts(&sandbox, &workspace)?;

    with_registry_mut(state_dir, |registry| {
        let sandbox = registry
            .sandboxes
            .get_mut(&sandbox.id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox.id))?;
        sandbox.workspaces.remove(&workspace_id);
        Ok(())
    })?;

    Ok(workspace_id)
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct BatchDestroyReport {
    pub removed: Vec<String>,
    pub errors: Vec<String>,
}

pub fn destroy_all_workspaces(state_dir: &std::path::Path) -> Result<BatchDestroyReport> {
    let plan = with_registry(state_dir, |registry| {
        let mut plan = Vec::new();
        for sandbox in registry.sandboxes.values() {
            for workspace in sandbox.workspaces.values() {
                plan.push((sandbox.metadata.clone(), workspace.clone()));
            }
        }
        Ok(plan)
    })?;

    if plan.is_empty() {
        return Ok(BatchDestroyReport::default());
    }

    let plan = Arc::new(plan);
    let worker_count = std::env::var("ENCLAVE_CLEANUP_WORKERS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(4)
        .clamp(1, 64)
        .min(plan.len());
    let queue = Arc::new(Mutex::new(VecDeque::from_iter(0..plan.len())));
    let results = Arc::new(Mutex::new(
        (0..plan.len()).map(|_| None).collect::<Vec<_>>(),
    ));

    thread::scope(|scope| {
        for _ in 0..worker_count {
            let queue = Arc::clone(&queue);
            let plan = Arc::clone(&plan);
            let results = Arc::clone(&results);
            scope.spawn(move || loop {
                let Some(index) = queue.lock().ok().and_then(|mut queue| queue.pop_front()) else {
                    break;
                };
                let (sandbox, workspace) = &plan[index];
                let result = cleanup_workspace_artifacts(sandbox, workspace).and_then(|()| {
                    let workspace_id = workspace.id.clone();
                    with_registry_mut(state_dir, |registry| {
                        if let Some(sandbox) = registry.sandboxes.get_mut(&sandbox.id) {
                            sandbox.workspaces.remove(&workspace_id);
                        }
                        Ok(workspace_id)
                    })
                });
                if let Ok(mut results) = results.lock() {
                    results[index] = Some(result);
                }
            });
        }
    });

    let results = Arc::try_unwrap(results)
        .map_err(|_| anyhow!("workspace cleanup result ownership leaked"))?
        .into_inner()
        .map_err(|_| anyhow!("workspace cleanup result lock poisoned"))?;
    let mut report = BatchDestroyReport::default();
    for result in results.into_iter().flatten() {
        match result {
            Ok(workspace_id) => report.removed.push(workspace_id),
            Err(error) => report.errors.push(format!("{error:#}")),
        }
    }
    Ok(report)
}

fn cleanup_workspace_artifacts(
    sandbox: &SandboxMetadata,
    workspace: &WorkspaceMetadata,
) -> Result<()> {
    if !std::path::Path::new(&sandbox.workspaces_path).exists() {
        return Ok(());
    }
    let workspace_path = validated_workspace_dir_from_metadata(sandbox, workspace)?;
    let mut errors = Vec::new();

    if let Some(pid) = workspace.runtime_pid {
        if let Err(err) = session::stop_session(pid, workspace.runtime_starttime_ticks) {
            errors.push(format!("stop runtime {}: {err:#}", pid));
        }
        remove_workspace_cgroups(sandbox, pid);
    }
    if let Some(ip) = workspace.assigned_ip.as_deref() {
        network::teardown_workspace_network(ip, &workspace.id);
    }

    let storage_unmounted = match crate::workspace::ensure_workspace_storage_unmounted(workspace) {
        Ok(()) => true,
        Err(err) => {
            errors.push(format!("unmount workspace storage: {err:#}"));
            false
        }
    };

    let mut artifacts = vec![
        workspace_path.join("workspace.json"),
        workspace_path.join("fs.img"),
        workspace_path.join("ns"),
        workspace_path.join("home-upper"),
        workspace_path.join("home-work"),
        workspace_path.join("home-merged"),
        workspace_path.join("runtime"),
    ];
    if storage_unmounted {
        artifacts.push(workspace_path.join("fs"));
    }

    for artifact in artifacts {
        if let Err(err) = remove_path_if_present(&artifact) {
            errors.push(format!("remove {}: {err:#}", artifact.display()));
        }
    }
    if storage_unmounted {
        if let Err(err) = remove_path_if_present(&workspace_path) {
            errors.push(format!("remove {}: {err:#}", workspace_path.display()));
        }
        if workspace_path.exists() {
            errors.push(format!(
                "workspace directory {} still exists",
                workspace_path.display()
            ));
        }
    } else {
        errors.push(format!(
            "workspace directory {} retained until all managed mounts are absent",
            workspace_path.display()
        ));
    }

    if errors.is_empty() {
        Ok(())
    } else {
        bail!(
            "workspace '{}' cleanup incomplete: {}",
            workspace.id,
            errors.join("; ")
        )
    }
}

fn remove_path_if_present(path: &std::path::Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            fs::remove_dir_all(path)
                .with_context(|| format!("failed to remove directory {}", path.display()))?;
        }
        Ok(_) => {
            fs::remove_file(path)
                .with_context(|| format!("failed to remove file {}", path.display()))?;
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(err).with_context(|| format!("failed to inspect {}", path.display()))
        }
    }
    Ok(())
}

pub fn update_workspace_definition(
    state_dir: &std::path::Path,
    sandbox_selector: &str,
    workspace_selector: &str,
    auth_providers: Option<Vec<String>>,
    env_tokens: Option<Vec<String>>,
    published_ports: Option<Vec<PublishedPortSpec>>,
    limits_update: WorkspaceLimitsUpdate,
) -> Result<WorkspaceMetadata> {
    let auth_providers = auth_providers
        .map(crate::workspace::create::normalize_auth_providers)
        .transpose()?;
    let env_tokens = env_tokens
        .map(crate::workspace::create::normalize_env_tokens)
        .transpose()?;
    let published_ports = published_ports
        .map(|ports| {
            crate::workspace::validate_published_ports(&ports)?;
            Ok::<Vec<PublishedPortSpec>, anyhow::Error>(ports)
        })
        .transpose()?;

    with_registry_mut(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, sandbox_selector)?;
        let sandbox = registry
            .sandboxes
            .get_mut(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_id))?;
        let workspace_id = resolve_workspace_id(sandbox, workspace_selector)?;
        let workspace = sandbox
            .workspaces
            .get_mut(&workspace_id)
            .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_id))?;

        if limits_update.disk_bytes.is_some() {
            bail!(
                "workspace disk allocation changes require `workspace resize`; metadata updates cannot resize fs.img"
            );
        }

        let mut changed = false;
        if let Some(auth_providers) = auth_providers {
            if workspace.auth_providers != auth_providers {
                workspace.auth_providers = auth_providers;
                changed = true;
            }
        }
        if let Some(env_tokens) = env_tokens {
            if workspace.env_tokens != env_tokens {
                workspace.env_tokens = env_tokens;
                changed = true;
            }
        }
        if let Some(published_ports) = published_ports {
            if workspace.published_ports != published_ports {
                workspace.published_ports = published_ports;
                changed = true;
            }
        }
        if let Some(clear_tmp_on_restart) = limits_update.clear_tmp_on_restart {
            if workspace.clear_tmp_on_restart != clear_tmp_on_restart {
                workspace.clear_tmp_on_restart = clear_tmp_on_restart;
                changed = true;
            }
        }
        changed |= workspace.limits.apply_update(&limits_update)?;
        crate::workspace::validate_workspace_storage_limits(
            workspace.home_mount_source_path.as_deref(),
            workspace.limits.disk_bytes,
        )?;

        if changed {
            crate::workspace::create_workspace_storage(workspace)?;
            persist_workspace_metadata(workspace)?;
        }

        Ok(workspace.clone())
    })
}

pub fn resize_workspace_disk_with_security(
    state_dir: &std::path::Path,
    sandbox_selector: &str,
    workspace_selector: &str,
    new_disk_bytes: u64,
    apparmor_profile: Option<&str>,
    selinux_label: Option<&str>,
) -> Result<WorkspaceResizeResult> {
    with_registry_mut(state_dir, |registry| {
        let used_ips = collect_all_used_ip_octets(registry);
        let sandbox_id = resolve_sandbox_id(registry, sandbox_selector)?;
        let sandbox = registry
            .sandboxes
            .get_mut(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_selector))?;
        if sandbox.metadata.status != SandboxStatus::Running {
            bail!(
                "sandbox '{}' is stopped; start it before resizing a workspace",
                sandbox.metadata.id
            );
        }

        let workspace_id = resolve_workspace_id(sandbox, workspace_selector)?;
        let current = sandbox
            .workspaces
            .get(&workspace_id)
            .cloned()
            .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_selector))?;
        let previous_disk_bytes = current.limits.disk_bytes.ok_or_else(|| {
            anyhow!(
                "workspace '{}' has no Enclave-managed disk allocation; configure disk_mb when creating it",
                current.name
            )
        })?;
        if current.home_mount_source_path.is_some() {
            bail!(
                "workspace '{}' uses a host-backed workspace directory; disk resize is only supported for Enclave-managed storage",
                current.name
            );
        }
        if new_disk_bytes == previous_disk_bytes {
            return Ok(WorkspaceResizeResult {
                workspace_id,
                workspace_name: current.name,
                previous_disk_bytes,
                new_disk_bytes,
                restarted: false,
            });
        }

        let was_running = current.status == WorkspaceStatus::Running;
        if was_running {
            if let Some(pid) = current.runtime_pid {
                session::stop_session(pid, current.runtime_starttime_ticks).with_context(|| {
                    format!(
                        "failed to stop workspace '{}' before resizing",
                        current.name
                    )
                })?;
            }
            set_workspace_stopped(sandbox, &workspace_id)?;
        }

        crate::workspace::ensure_workspace_storage_unmounted(&current).with_context(|| {
            format!(
                "failed to unmount workspace '{}' before resizing",
                current.name
            )
        })?;

        let resize = super::storage::increase_workspace_disk_allocation(&current, new_disk_bytes)?;
        let resized_workspace = {
            let workspace = sandbox
                .workspaces
                .get_mut(&workspace_id)
                .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_id))?;
            workspace.limits.disk_bytes = Some(resize.new_bytes);
            persist_workspace_metadata(workspace)?;
            workspace.clone()
        };

        let restarted = if was_running {
            let sandbox_snapshot = sandbox.metadata.clone();
            let workspace_snapshot = resized_workspace.clone();
            let started = launch_workspace_runtime(
                state_dir,
                &sandbox_snapshot,
                &workspace_snapshot,
                apparmor_profile,
                selinux_label,
                NetworkStartPlan::AllocateFromUsedIps(used_ips),
            )?;
            let workspace = sandbox
                .workspaces
                .get_mut(&workspace_id)
                .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_id))?;
            workspace.status = WorkspaceStatus::Running;
            workspace.runtime_pid = Some(started.pid);
            workspace.runtime_starttime_ticks = Some(started.starttime_ticks);
            workspace.assigned_ip = Some(started.assigned_ip);
            normalize_namespace_ref_paths(workspace);
            session::write_namespace_ref_values(workspace, &started.mount_ns, &started.pid_ns)?;
            persist_workspace_metadata(workspace)?;
            true
        } else {
            false
        };

        Ok(WorkspaceResizeResult {
            workspace_id,
            workspace_name: current.name,
            previous_disk_bytes,
            new_disk_bytes: resize.new_bytes,
            restarted,
        })
    })
}

pub fn resize_workspace_disk(
    state_dir: &std::path::Path,
    sandbox_selector: &str,
    workspace_selector: &str,
    new_disk_bytes: u64,
) -> Result<WorkspaceResizeResult> {
    resize_workspace_disk_with_security(
        state_dir,
        sandbox_selector,
        workspace_selector,
        new_disk_bytes,
        None,
        None,
    )
}

pub fn start_workspace(
    state_dir: &std::path::Path,
    sandbox_selector: &str,
    workspace_selector: &str,
) -> Result<WorkspaceMetadata> {
    start_workspace_with_security(state_dir, sandbox_selector, workspace_selector, None, None)
}

pub fn start_workspace_with_security(
    state_dir: &std::path::Path,
    sandbox_selector: &str,
    workspace_selector: &str,
    apparmor_profile: Option<&str>,
    selinux_label: Option<&str>,
) -> Result<WorkspaceMetadata> {
    let (sandbox_id, workspace_id, sandbox_snapshot, mut workspace_snapshot, used_ips) =
        with_registry(state_dir, |registry| {
            let sandbox_id = resolve_sandbox_id(registry, sandbox_selector)?;
            let sandbox = registry
                .sandboxes
                .get(&sandbox_id)
                .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_id))?;
            if sandbox.metadata.status != SandboxStatus::Running {
                bail!(
                    "sandbox '{}' is stopped; start sandbox first",
                    sandbox.metadata.id
                );
            }
            let workspace_id = resolve_workspace_id(sandbox, workspace_selector)?;
            let mut workspace_snapshot = sandbox
                .workspaces
                .get(&workspace_id)
                .cloned()
                .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_id))?;
            workspace_snapshot.sandbox_rootfs_path = effective_rootfs_path(&sandbox.metadata);
            Ok((
                sandbox_id,
                workspace_id,
                sandbox.metadata.clone(),
                workspace_snapshot,
                collect_all_used_ip_octets(registry),
            ))
        })?;

    let expected_runtime = workspace_snapshot
        .runtime_pid
        .zip(workspace_snapshot.runtime_starttime_ticks);
    if workspace_snapshot.status == WorkspaceStatus::Running {
        if let Some((pid, starttime)) = workspace_snapshot
            .runtime_pid
            .zip(workspace_snapshot.runtime_starttime_ticks)
        {
            if session::process_matches(pid, Some(starttime)) {
                let (mount_ns, pid_ns) = session::read_namespace_refs(pid)?;
                return with_registry_mut(state_dir, |registry| {
                    let sandbox = registry
                        .sandboxes
                        .get_mut(&sandbox_id)
                        .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_id))?;
                    let workspace = sandbox
                        .workspaces
                        .get_mut(&workspace_id)
                        .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_id))?;
                    if workspace.runtime_pid != Some(pid)
                        || workspace.runtime_starttime_ticks != Some(starttime)
                    {
                        bail!("workspace runtime identity changed while refreshing namespace refs");
                    }
                    normalize_namespace_ref_paths(workspace);
                    session::write_namespace_ref_values(workspace, &mount_ns, &pid_ns)?;
                    Ok(workspace.clone())
                });
            }
        }
        workspace_snapshot.status = WorkspaceStatus::Stopped;
        workspace_snapshot.runtime_pid = None;
        workspace_snapshot.runtime_starttime_ticks = None;
        workspace_snapshot.assigned_ip = None;
    }

    let started = launch_workspace_runtime(
        state_dir,
        &sandbox_snapshot,
        &workspace_snapshot,
        apparmor_profile,
        selinux_label,
        NetworkStartPlan::AllocateFromUsedIps(used_ips),
    )?;

    let commit = with_registry_mut(state_dir, |registry| {
        let sandbox = registry
            .sandboxes
            .get_mut(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_id))?;
        let workspace = sandbox
            .workspaces
            .get_mut(&workspace_id)
            .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_id))?;
        if workspace.status == WorkspaceStatus::Running
            && workspace.runtime_pid.zip(workspace.runtime_starttime_ticks) != expected_runtime
        {
            bail!("workspace became running while startup was in progress");
        }
        workspace.sandbox_rootfs_path = workspace_snapshot.sandbox_rootfs_path.clone();
        workspace.status = WorkspaceStatus::Running;
        workspace.runtime_pid = Some(started.pid);
        workspace.runtime_starttime_ticks = Some(started.starttime_ticks);
        workspace.assigned_ip = Some(started.assigned_ip.clone());
        normalize_namespace_ref_paths(workspace);
        session::write_namespace_ref_values(workspace, &started.mount_ns, &started.pid_ns)?;

        let metadata_path = PathBuf::from(&workspace.workspace_path).join("workspace.json");
        let metadata_raw = serde_json::to_string_pretty(workspace)?;
        crate::fsutil::write_file_atomic(&metadata_path, metadata_raw.as_bytes(), 0o600)
            .with_context(|| {
                format!(
                    "failed to write workspace metadata {}",
                    metadata_path.display()
                )
            })?;
        Ok(workspace.clone())
    });

    match commit {
        Ok(metadata) => Ok(metadata),
        Err(error) => {
            let _ = session::stop_session(started.pid, Some(started.starttime_ticks));
            network::teardown_workspace_network(&started.assigned_ip, &workspace_id);
            let _ = crate::workspace::ensure_workspace_storage_unmounted(&workspace_snapshot);
            Err(error)
        }
    }
}

fn launch_workspace_runtime(
    state_dir: &std::path::Path,
    sandbox_snapshot: &SandboxMetadata,
    workspace_snapshot: &WorkspaceMetadata,
    apparmor_profile: Option<&str>,
    selinux_label: Option<&str>,
    network_plan: NetworkStartPlan,
) -> Result<WorkspaceRuntimeStart> {
    crate::workspace::ensure_workspace_storage_ready(workspace_snapshot)?;
    let session_info = session::start_session(workspace_snapshot, apparmor_profile, selinux_label)?;
    if let Err(err) =
        apply_workspace_runtime_constraints(sandbox_snapshot, workspace_snapshot, session_info.pid)
    {
        if let Err(stop_err) =
            session::stop_session(session_info.pid, Some(session_info.starttime_ticks))
        {
            tracing::warn!(
                "failed to stop workspace session {} after cgroup setup failure: {stop_err:#}",
                session_info.pid
            );
        }
        return Err(err).context("failed to apply workspace cgroup limits");
    }

    let workspace_rootfs_path = format!("/proc/{}/root", session_info.pid);
    let auth_manager = crate::auth::AuthManager::new(state_dir.to_path_buf());
    if let Err(err) = auth_manager.sync_workspace_auth(
        &workspace_rootfs_path,
        &workspace_snapshot.auth_providers,
        &workspace_snapshot.env_tokens,
    ) {
        remove_workspace_cgroups(sandbox_snapshot, session_info.pid);
        if let Err(stop_err) =
            session::stop_session(session_info.pid, Some(session_info.starttime_ticks))
        {
            tracing::warn!(
                "failed to stop workspace session {} after auth sync failure: {stop_err:#}",
                session_info.pid
            );
        }
        return Err(err)
            .context("failed to sync workspace auth; attempted to stop workspace session");
    }

    let workspace_rootfs = PathBuf::from(format!("/proc/{}/root", session_info.pid));
    let assigned_ip = match network_plan {
        NetworkStartPlan::AllocateFromUsedIps(used_ips) => {
            match network::setup_workspace_network(
                session_info.pid,
                &used_ips,
                &workspace_rootfs,
                &workspace_snapshot.id,
            ) {
                Ok(ip) => ip,
                Err(err) => {
                    remove_workspace_cgroups(sandbox_snapshot, session_info.pid);
                    if let Err(stop_err) =
                        session::stop_session(session_info.pid, Some(session_info.starttime_ticks))
                    {
                        tracing::warn!(
                            "failed to stop workspace session {} after network setup failure: {stop_err:#}",
                            session_info.pid
                        );
                    }
                    return Err(err).context(
                        "failed to attach or validate workspace networking; aborted workspace startup",
                    );
                }
            }
        }
    };

    Ok(WorkspaceRuntimeStart {
        pid: session_info.pid,
        starttime_ticks: session_info.starttime_ticks,
        mount_ns: session_info.mount_ns,
        pid_ns: session_info.pid_ns,
        assigned_ip,
    })
}

pub fn stop_workspace(
    state_dir: &std::path::Path,
    sandbox_selector: &str,
    workspace_selector: &str,
) -> Result<WorkspaceMetadata> {
    let (sandbox_id, workspace_id, current) = with_registry(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, sandbox_selector)?;
        let sandbox = registry
            .sandboxes
            .get(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_id))?;
        let workspace_id = resolve_workspace_id(sandbox, workspace_selector)?;
        let current = sandbox
            .workspaces
            .get(&workspace_id)
            .cloned()
            .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_id))?;
        Ok((sandbox_id, workspace_id, current))
    })?;

    if let Some(pid) = current.runtime_pid {
        session::stop_session(pid, current.runtime_starttime_ticks)?;
    }

    with_registry_mut(state_dir, |registry| {
        let sandbox = registry
            .sandboxes
            .get_mut(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_id))?;
        let latest = sandbox
            .workspaces
            .get(&workspace_id)
            .cloned()
            .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_id))?;
        if latest.runtime_pid != current.runtime_pid
            || latest.runtime_starttime_ticks != current.runtime_starttime_ticks
        {
            bail!(
                "workspace '{}' changed while it was stopping; refusing stale metadata commit",
                workspace_id
            );
        }
        set_workspace_stopped(sandbox, &workspace_id)?;
        let result = sandbox
            .workspaces
            .get(&workspace_id)
            .cloned()
            .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_id))?;
        Ok(result)
    })
}

pub(crate) fn stop_running_workspaces_in_sandbox(
    state_dir: &std::path::Path,
    sandbox_selector: &str,
) -> Result<Vec<String>> {
    let running = with_registry(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, sandbox_selector)?;
        let sandbox = registry
            .sandboxes
            .get(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_id))?;
        Ok(sandbox
            .workspaces
            .values()
            .filter(|workspace| workspace.status == WorkspaceStatus::Running)
            .cloned()
            .collect::<Vec<_>>())
    })?;

    let stop_targets = running
        .iter()
        .filter_map(|workspace| {
            workspace
                .runtime_pid
                .map(|pid| (pid, workspace.runtime_starttime_ticks))
        })
        .collect::<Vec<_>>();
    let stop_result = session::stop_sessions_batch(&stop_targets)?;

    let mut stopped_ids = BTreeSet::new();
    let mut cleanup_jobs = Vec::new();
    let mut failed_ids = Vec::new();
    for workspace in &running {
        let failed = workspace
            .runtime_pid
            .is_some_and(|pid| stop_result.failed_pids.contains(&pid));
        if failed {
            failed_ids.push(workspace.id.clone());
        } else {
            stopped_ids.insert(workspace.id.clone());
        }
    }

    with_registry_mut(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, sandbox_selector)?;
        let sandbox = registry
            .sandboxes
            .get_mut(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_id))?;

        for workspace_id in &stopped_ids {
            if let Some(job) = mark_workspace_stopped(sandbox, workspace_id)? {
                cleanup_jobs.push(job);
            }
        }
        Ok(())
    })?;

    run_workspace_stop_cleanups(cleanup_jobs);

    with_registry(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, sandbox_selector)?;
        let sandbox = registry
            .sandboxes
            .get(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_selector))?;
        remove_sandbox_cgroup_if_idle(sandbox);
        Ok(())
    })?;

    Ok(failed_ids)
}

pub fn list_workspace_items(
    state_dir: &std::path::Path,
    sandbox_selector: &str,
) -> Result<Vec<WorkspaceListItem>> {
    with_registry(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, sandbox_selector)?;
        let sandbox = registry
            .sandboxes
            .get(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_id))?;

        let mut items = Vec::new();
        for workspace in sandbox.workspaces.values() {
            items.push(WorkspaceListItem {
                id: workspace.id.clone(),
                name: workspace.name.clone(),
                status: workspace.status.clone(),
            });
        }
        items.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(items)
    })
}

pub fn workspace_status(
    state_dir: &std::path::Path,
    sandbox_selector: &str,
    workspace_selector: &str,
    runtime_published_ports: &[PublishedPortStatus],
) -> Result<WorkspaceStatusReport> {
    with_registry(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, sandbox_selector)?;
        let sandbox = registry
            .sandboxes
            .get(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_id))?;
        let workspace_id = resolve_workspace_id(sandbox, workspace_selector)?;
        let workspace = sandbox
            .workspaces
            .get(&workspace_id)
            .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_id))?;

        let runtime_is_active = workspace_runtime_is_active(workspace);
        let active_process_count = if runtime_is_active {
            if let Some(pid) = workspace.runtime_pid {
                session::count_processes_in_pid_namespace(pid).unwrap_or(0)
            } else {
                0
            }
        } else {
            0
        };
        let resource_usage = if runtime_is_active {
            if let Some(pid) = workspace.runtime_pid {
                session::process_resource_usage(pid).ok()
            } else {
                None
            }
        } else {
            None
        };

        Ok(WorkspaceStatusReport {
            id: workspace.id.clone(),
            name: workspace.name.clone(),
            created_at: workspace.created_at.clone(),
            allocated_path: workspace.workspace_path.clone(),
            status: if workspace.status == WorkspaceStatus::Running && !runtime_is_active {
                WorkspaceStatus::Stopped
            } else {
                workspace.status.clone()
            },
            active_process_count,
            resource_usage,
            limits: workspace.limits.clone(),
            sandbox_limits: sandbox.metadata.limits.clone(),
            published_ports: merge_published_port_statuses(
                &workspace.published_ports,
                runtime_published_ports,
            ),
        })
    })
}

pub fn workspace_metadata(
    state_dir: &std::path::Path,
    sandbox_selector: &str,
    workspace_selector: &str,
) -> Result<WorkspaceMetadata> {
    with_registry(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, sandbox_selector)?;
        let sandbox = registry
            .sandboxes
            .get(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_id))?;
        let workspace_id = resolve_workspace_id(sandbox, workspace_selector)?;
        sandbox
            .workspaces
            .get(&workspace_id)
            .cloned()
            .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_id))
    })
}

pub(crate) fn resolve_workspace_id(sandbox: &RegistrySandbox, selector: &str) -> Result<String> {
    if sandbox.workspaces.contains_key(selector) {
        return Ok(selector.to_string());
    }

    let mut matches = Vec::new();
    for (id, workspace) in &sandbox.workspaces {
        if workspace.name == selector {
            matches.push(id.clone());
        }
    }

    match matches.len() {
        0 => bail!(
            "workspace '{}' not found in sandbox '{}'",
            selector,
            sandbox.metadata.id
        ),
        1 => Ok(matches.remove(0)),
        _ => bail!(
            "workspace name '{}' is ambiguous in sandbox '{}'; use id instead (matches: {})",
            selector,
            sandbox.metadata.id,
            matches.join(", ")
        ),
    }
}

pub(crate) fn set_workspace_stopped(
    sandbox: &mut RegistrySandbox,
    workspace_id: &str,
) -> Result<()> {
    if let Some(job) = mark_workspace_stopped(sandbox, workspace_id)? {
        run_workspace_stop_cleanup(job);
    }
    remove_sandbox_cgroup_if_idle(sandbox);
    Ok(())
}

struct WorkspaceStopCleanup {
    sandbox: SandboxMetadata,
    workspace: WorkspaceMetadata,
}

fn mark_workspace_stopped(
    sandbox: &mut RegistrySandbox,
    workspace_id: &str,
) -> Result<Option<WorkspaceStopCleanup>> {
    let workspace = sandbox
        .workspaces
        .get_mut(workspace_id)
        .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_id))?;
    let cleanup = WorkspaceStopCleanup {
        sandbox: sandbox.metadata.clone(),
        workspace: workspace.clone(),
    };

    workspace.status = WorkspaceStatus::Stopped;
    workspace.runtime_pid = None;
    workspace.runtime_starttime_ticks = None;
    workspace.assigned_ip = None;
    clear_workspace_namespace_refs(workspace);
    let pid_file = session::runtime_pid_file(workspace);
    if let Err(err) = fs::remove_file(&pid_file) {
        if err.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!("failed to remove {}: {err:#}", pid_file.display());
        }
    }
    let ready_file = session::runtime_ready_file(workspace);
    if let Err(err) = fs::remove_file(&ready_file) {
        if err.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!("failed to remove {}: {err:#}", ready_file.display());
        }
    }

    let metadata_path = PathBuf::from(&workspace.workspace_path).join("workspace.json");
    let metadata_raw = serde_json::to_string_pretty(workspace)?;
    crate::fsutil::write_file_atomic(&metadata_path, metadata_raw.as_bytes(), 0o600).with_context(
        || {
            format!(
                "failed to persist workspace state to {}",
                metadata_path.display()
            )
        },
    )?;
    Ok(Some(cleanup))
}

pub(crate) fn reconcile_workspace_runtime_state(workspace: &mut WorkspaceMetadata) -> Result<bool> {
    if workspace.status != WorkspaceStatus::Running {
        let stale_runtime_state = workspace.runtime_pid.is_some()
            || workspace.runtime_starttime_ticks.is_some()
            || workspace.assigned_ip.is_some()
            || workspace.namespace_refs.mount != "unassigned"
            || workspace.namespace_refs.pid != "unassigned"
            || session::namespace_ref_files_exist(workspace);
        if !stale_runtime_state {
            return Ok(false);
        }
        workspace.runtime_pid = None;
        workspace.runtime_starttime_ticks = None;
        workspace.assigned_ip = None;
        clear_workspace_namespace_refs(workspace);
        persist_workspace_metadata(workspace)?;
        return Ok(true);
    }

    let runtime_is_live = workspace
        .runtime_pid
        .zip(workspace.runtime_starttime_ticks)
        .is_some_and(|(pid, starttime)| session::process_matches(pid, Some(starttime)));
    if !runtime_is_live {
        workspace.status = WorkspaceStatus::Stopped;
        workspace.runtime_pid = None;
        workspace.runtime_starttime_ticks = None;
        workspace.assigned_ip = None;
        clear_workspace_namespace_refs(workspace);
        persist_workspace_metadata(workspace)?;
        return Ok(true);
    }

    let pid = workspace.runtime_pid.expect("live runtime has a pid");
    if session::namespace_refs_match_runtime(workspace, pid) {
        return Ok(false);
    }

    let (mount_ns, pid_ns) = session::read_namespace_refs(pid)?;
    normalize_namespace_ref_paths(workspace);
    session::write_namespace_ref_values(workspace, &mount_ns, &pid_ns)?;
    persist_workspace_metadata(workspace)?;
    Ok(true)
}

pub(crate) fn workspace_runtime_is_active(workspace: &WorkspaceMetadata) -> bool {
    workspace.status == WorkspaceStatus::Running
        && workspace
            .runtime_pid
            .zip(workspace.runtime_starttime_ticks)
            .is_some_and(|(pid, starttime)| {
                session::process_matches(pid, Some(starttime))
                    && session::namespace_refs_match_runtime(workspace, pid)
            })
}

fn clear_workspace_namespace_refs(workspace: &mut WorkspaceMetadata) {
    if let Err(err) = session::clear_namespace_ref_files(workspace) {
        tracing::warn!(
            "failed to clear namespace refs for workspace {}: {err:#}",
            workspace.id
        );
    }
    workspace.namespace_refs = Default::default();
}

fn run_workspace_stop_cleanups(cleanups: Vec<WorkspaceStopCleanup>) {
    if cleanups.is_empty() {
        return;
    }

    let worker_count = cleanup_worker_count(cleanups.len());
    let queue = Arc::new(Mutex::new(VecDeque::from(cleanups)));
    let handles = (0..worker_count)
        .map(|worker_id| {
            let queue = Arc::clone(&queue);
            thread::Builder::new()
                .name(format!("enclave-workspace-cleanup-{worker_id}"))
                .spawn(move || loop {
                    let cleanup = match queue.lock() {
                        Ok(mut queue) => queue.pop_front(),
                        Err(_) => return,
                    };
                    let Some(cleanup) = cleanup else {
                        return;
                    };
                    run_workspace_stop_cleanup(cleanup);
                })
                .expect("failed to spawn workspace cleanup worker")
        })
        .collect::<Vec<_>>();

    for handle in handles {
        if let Err(err) = handle.join() {
            tracing::warn!("workspace cleanup thread panicked: {:?}", err);
        }
    }
}

fn cleanup_worker_count(job_count: usize) -> usize {
    std::env::var("ENCLAVE_CLEANUP_WORKERS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| (1..=64).contains(value))
        .unwrap_or(4)
        .min(job_count)
}

fn run_workspace_stop_cleanup(cleanup: WorkspaceStopCleanup) {
    let workspace = cleanup.workspace;
    let sandbox = cleanup.sandbox;

    if let Some(pid) = workspace.runtime_pid {
        remove_workspace_cgroups(&sandbox, pid);
    }

    if let Some(ref ip) = workspace.assigned_ip {
        network::teardown_workspace_network(ip, &workspace.id);
    }
    match crate::workspace::ensure_workspace_storage_unmounted(&workspace) {
        Ok(()) if workspace.clear_tmp_on_restart => {
            if let Err(err) = crate::workspace::reset_workspace_tmp(&workspace) {
                tracing::warn!(
                    "failed to reset workspace /tmp for {}: {err:#}",
                    workspace.id
                );
            }
        }
        Ok(()) => {}
        Err(err) => tracing::warn!(
            "failed to unmount quota-backed workspace storage for {}: {err:#}",
            workspace.id
        ),
    }
}

fn remove_sandbox_cgroup_if_idle(sandbox: &RegistrySandbox) {
    if sandbox
        .workspaces
        .values()
        .all(|item| item.status != WorkspaceStatus::Running)
        && !sandbox.metadata.limits.has_limits()
    {
        remove_sandbox_cgroup(&sandbox.metadata);
    }
}

fn workspace_cgroup_name(pid: u32) -> String {
    format!("enclave-ws-{pid}")
}

pub(crate) fn sync_workspace_runtime_limits(
    state_dir: &std::path::Path,
    sandbox_selector: &str,
    workspace_selector: &str,
) -> Result<WorkspaceMetadata> {
    let (sandbox, workspace) = with_registry(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, sandbox_selector)?;
        let sandbox = registry
            .sandboxes
            .get(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_selector))?;
        let workspace_id = resolve_workspace_id(sandbox, workspace_selector)?;
        let workspace = sandbox
            .workspaces
            .get(&workspace_id)
            .cloned()
            .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_selector))?;
        Ok((sandbox.metadata.clone(), workspace))
    })?;

    if workspace_runtime_is_active(&workspace) {
        let pid = workspace
            .runtime_pid
            .expect("active workspace has a runtime pid");
        apply_workspace_runtime_constraints(&sandbox, &workspace, pid)?;
    }

    Ok(workspace)
}

pub(crate) fn sync_sandbox_runtime_limits(
    state_dir: &std::path::Path,
    sandbox_selector: &str,
) -> Result<()> {
    let (sandbox, workspaces) = with_registry(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, sandbox_selector)?;
        let sandbox = registry
            .sandboxes
            .get(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_selector))?;
        let workspaces = sandbox.workspaces.values().cloned().collect::<Vec<_>>();
        Ok((sandbox.metadata.clone(), workspaces))
    })?;

    if sandbox.status != SandboxStatus::Running {
        return Ok(());
    }

    if workspaces
        .iter()
        .all(|workspace| !workspace_runtime_is_active(workspace))
    {
        let sandbox_config = build_sandbox_cgroup_config(&sandbox)?;
        if crate::sandbox::cgroup::is_cgroup_v2_available() {
            if sandbox_config.has_limits() {
                crate::sandbox::cgroup::ensure_sandbox_cgroup(
                    &crate::sandbox::cgroup::sandbox_cgroup_name(&sandbox.id),
                    &sandbox_config,
                    true,
                )?;
            } else {
                remove_sandbox_cgroup(&sandbox);
            }
        }
        return Ok(());
    }

    for workspace in workspaces {
        if !workspace_runtime_is_active(&workspace) {
            continue;
        }
        let pid = workspace
            .runtime_pid
            .expect("active workspace has a runtime pid");
        apply_workspace_runtime_constraints(&sandbox, &workspace, pid)?;
    }

    Ok(())
}

fn normalize_namespace_ref_paths(workspace: &mut WorkspaceMetadata) {
    let (mount_ref_path, pid_ref_path) = session::namespace_ref_paths(workspace);
    workspace.namespace_refs.mount = mount_ref_path.to_string_lossy().to_string();
    workspace.namespace_refs.pid = pid_ref_path.to_string_lossy().to_string();
}

fn persist_workspace_metadata(workspace: &WorkspaceMetadata) -> Result<()> {
    let metadata_path = PathBuf::from(&workspace.workspace_path).join("workspace.json");
    let metadata_raw = serde_json::to_string_pretty(workspace)?;
    crate::fsutil::write_file_atomic(&metadata_path, metadata_raw.as_bytes(), 0o600).with_context(
        || {
            format!(
                "failed to persist workspace metadata to {}",
                metadata_path.display()
            )
        },
    )
}

fn validated_workspace_dir_from_metadata(
    sandbox: &SandboxMetadata,
    workspace: &WorkspaceMetadata,
) -> Result<PathBuf> {
    let sandbox_base = PathBuf::from(&sandbox.sandbox_path);
    let workspace_root = PathBuf::from(&sandbox.workspaces_path);
    let sandbox_dir =
        crate::fsutil::ensure_path_within(&sandbox_base, &workspace_root, "workspace root")?;
    let workspace_dir = PathBuf::from(&workspace.workspace_path);
    crate::fsutil::ensure_path_within(&sandbox_dir, &workspace_dir, "workspace directory")
}

fn collect_all_used_ip_octets(
    registry: &crate::registry::Registry,
) -> std::collections::BTreeSet<u8> {
    let ips = registry
        .sandboxes
        .values()
        .flat_map(|s| s.workspaces.values())
        .filter(|workspace| workspace_runtime_is_active(workspace))
        .filter_map(|ws| ws.assigned_ip.as_deref());
    network::collect_used_ips(ips)
}

fn apply_workspace_runtime_constraints(
    sandbox: &SandboxMetadata,
    workspace: &WorkspaceMetadata,
    pid: u32,
) -> Result<()> {
    ensure_workspace_cgroup_hierarchy(sandbox, workspace, pid)
}

fn ensure_workspace_cgroup_hierarchy(
    sandbox: &SandboxMetadata,
    workspace: &WorkspaceMetadata,
    pid: u32,
) -> Result<()> {
    let sandbox_config = build_sandbox_cgroup_config(sandbox)?;
    let workspace_config = build_workspace_cgroup_config(workspace)?;
    let cgroup_v2_available = crate::sandbox::cgroup::is_cgroup_v2_available();

    if sandbox.limits.has_limits() && !cgroup_v2_available {
        bail!(
            "sandbox '{}' declares resource limits but cgroup v2 is unavailable on this host",
            sandbox.id
        );
    }
    if workspace.limits.cpu_percent_requires_cgroup() && !cgroup_v2_available {
        bail!(
            "workspace '{}' declares cpu_percent but cgroup v2 is unavailable on this host",
            workspace.id
        );
    }
    if !cgroup_v2_available || (!sandbox_config.has_limits() && !workspace_config.has_limits()) {
        return Ok(());
    }

    let sandbox_path = crate::sandbox::cgroup::ensure_sandbox_cgroup(
        &crate::sandbox::cgroup::sandbox_cgroup_name(&sandbox.id),
        &sandbox_config,
        true,
    )?
    .ok_or_else(|| anyhow!("failed to prepare sandbox cgroup for '{}'", sandbox.id))?;
    let workspace_name = workspace_cgroup_name(pid);
    if let Some(workspace_path) = crate::sandbox::cgroup::ensure_workspace_cgroup(
        &sandbox_path,
        &workspace_name,
        &workspace_config,
        true,
    )? {
        crate::sandbox::cgroup::add_process_to_cgroup(&workspace_path, pid)?;
    }
    if let Err(err) = crate::sandbox::cgroup::remove_workspace_cgroup(&workspace_name) {
        tracing::debug!(
            "legacy workspace cgroup cleanup skipped for {}: {err:#}",
            workspace.id
        );
    }
    Ok(())
}

fn build_sandbox_cgroup_config(
    sandbox: &SandboxMetadata,
) -> Result<crate::sandbox::cgroup::CgroupConfig> {
    crate::sandbox::cgroup::CgroupConfig::from_limits(
        sandbox.limits.memory_bytes,
        sandbox.limits.cpu_percent,
        sandbox.limits.max_processes,
    )
}

fn build_workspace_cgroup_config(
    workspace: &WorkspaceMetadata,
) -> Result<crate::sandbox::cgroup::CgroupConfig> {
    crate::sandbox::cgroup::CgroupConfig::from_limits(
        workspace.limits.memory_bytes,
        workspace.limits.cpu_percent,
        workspace.limits.max_processes,
    )
}

fn remove_workspace_cgroups(sandbox: &SandboxMetadata, pid: u32) {
    let workspace_name = workspace_cgroup_name(pid);
    let sandbox_path = std::path::PathBuf::from("/sys/fs/cgroup")
        .join(crate::sandbox::cgroup::sandbox_cgroup_name(&sandbox.id))
        .join(&workspace_name);
    if let Err(err) = crate::sandbox::cgroup::remove_cgroup_path(&sandbox_path) {
        tracing::warn!(
            "failed to remove workspace cgroup '{}' during stop: {err:#}",
            sandbox_path.display()
        );
    }
    if let Err(err) = crate::sandbox::cgroup::remove_workspace_cgroup(&workspace_name) {
        tracing::debug!(
            "legacy workspace cgroup '{}' cleanup skipped: {err:#}",
            workspace_name
        );
    }
}

fn remove_sandbox_cgroup(sandbox: &SandboxMetadata) {
    let sandbox_path = std::path::PathBuf::from("/sys/fs/cgroup")
        .join(crate::sandbox::cgroup::sandbox_cgroup_name(&sandbox.id));
    if let Err(err) = crate::sandbox::cgroup::remove_cgroup_path(&sandbox_path) {
        tracing::debug!(
            "sandbox cgroup cleanup skipped for '{}': {err:#}",
            sandbox.id
        );
    }
}

#[cfg(test)]
#[path = "../../tests/src/workspace/control.rs"]
mod tests;
