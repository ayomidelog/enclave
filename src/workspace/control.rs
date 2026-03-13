use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};

use crate::network;
use crate::registry::{with_registry, with_registry_mut, RegistrySandbox};
use crate::sandbox::{effective_rootfs_path, resolve_sandbox_id, SandboxMetadata, SandboxStatus};

use super::ports::{merge_published_port_statuses, PublishedPortSpec, PublishedPortStatus};
use super::session;
use super::types::{
    WorkspaceLimitsUpdate, WorkspaceListItem, WorkspaceMetadata, WorkspaceStatus,
    WorkspaceStatusReport,
};

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
    with_registry_mut(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, sandbox_selector)?;
        let sandbox = registry
            .sandboxes
            .get_mut(&sandbox_id)
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

        if workspace.status == WorkspaceStatus::Running {
            if let Some(pid) = workspace.runtime_pid {
                session::stop_session(pid, workspace.runtime_starttime_ticks)?;
            }
            set_workspace_stopped(sandbox, &workspace_id)?;
        }

        let workspace_path = validated_workspace_dir(sandbox, &workspace)?;
        if let Err(err) = fs::remove_dir_all(&workspace_path) {
            if err.kind() != std::io::ErrorKind::NotFound {
                return Err(err).with_context(|| {
                    format!("failed to remove workspace {}", workspace_path.display())
                });
            }
        }

        sandbox.workspaces.remove(&workspace_id);
        Ok(workspace_id)
    })
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
        changed |= workspace.limits.apply_update(&limits_update)?;

        if changed {
            persist_workspace_metadata(workspace)?;
        }

        Ok(workspace.clone())
    })
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
    with_registry_mut(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, sandbox_selector)?;

        let (workspace_id, sandbox_snapshot, workspace_snapshot) = {
            let sandbox = registry
                .sandboxes
                .get_mut(&sandbox_id)
                .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_id))?;

            if sandbox.metadata.status != SandboxStatus::Running {
                bail!(
                    "sandbox '{}' is stopped; start sandbox first",
                    sandbox.metadata.id
                );
            }

            let workspace_id = resolve_workspace_id(sandbox, workspace_selector)?;
            let sandbox_rootfs_path = effective_rootfs_path(&sandbox.metadata);
            {
                let workspace = sandbox
                    .workspaces
                    .get_mut(&workspace_id)
                    .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_id))?;
                normalize_namespace_ref_paths(workspace);
                workspace.sandbox_rootfs_path = sandbox_rootfs_path.clone();
            }
            let current = sandbox
                .workspaces
                .get(&workspace_id)
                .cloned()
                .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_id))?;

            if current.status == WorkspaceStatus::Running {
                if let Some(pid) = current.runtime_pid {
                    if session::process_matches(pid, current.runtime_starttime_ticks) {
                        let (mount_ns, pid_ns) = session::read_namespace_refs(pid)?;
                        let workspace = sandbox
                            .workspaces
                            .get_mut(&workspace_id)
                            .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_id))?;
                        normalize_namespace_ref_paths(workspace);
                        session::write_namespace_ref_values(workspace, &mount_ns, &pid_ns)?;
                        return Ok(workspace.clone());
                    }
                }
                set_workspace_stopped(sandbox, &workspace_id)?;
            }

            let workspace_snapshot = sandbox
                .workspaces
                .get(&workspace_id)
                .cloned()
                .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_id))?;
            (workspace_id, sandbox.metadata.clone(), workspace_snapshot)
        };

        let session_info =
            session::start_session(&workspace_snapshot, apparmor_profile, selinux_label)?;
        if let Err(err) = apply_workspace_runtime_constraints(
            &sandbox_snapshot,
            &workspace_snapshot,
            session_info.pid,
        ) {
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
        let workspace_rootfs = format!("/proc/{}/root", session_info.pid);
        let auth_manager = crate::auth::AuthManager::new(state_dir.to_path_buf());
        if let Err(err) = auth_manager.sync_workspace_auth(
            &workspace_rootfs,
            &workspace_snapshot.auth_providers,
            &workspace_snapshot.env_tokens,
        ) {
            remove_workspace_cgroups(&sandbox_snapshot, session_info.pid);
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

        let used_ips = collect_all_used_ip_octets(registry);
        let rootfs_path = Path::new(&workspace_snapshot.sandbox_rootfs_path);
        let assigned_ip =
            match network::setup_workspace_network(session_info.pid, &used_ips, rootfs_path) {
                Ok(ip) => Some(ip),
                Err(err) => {
                    tracing::warn!(
                        "workspace networking setup failed \
                         (workspace will have no network connectivity): {err:#}"
                    );
                    None
                }
            };

        let sandbox = registry
            .sandboxes
            .get_mut(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_id))?;
        let workspace = sandbox
            .workspaces
            .get_mut(&workspace_id)
            .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_id))?;
        workspace.status = WorkspaceStatus::Running;
        workspace.runtime_pid = Some(session_info.pid);
        workspace.runtime_starttime_ticks = Some(session_info.starttime_ticks);
        workspace.assigned_ip = assigned_ip;
        normalize_namespace_ref_paths(workspace);
        session::write_namespace_ref_values(
            workspace,
            &session_info.mount_ns,
            &session_info.pid_ns,
        )?;

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
    })
}

pub fn stop_workspace(
    state_dir: &std::path::Path,
    sandbox_selector: &str,
    workspace_selector: &str,
) -> Result<WorkspaceMetadata> {
    with_registry_mut(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, sandbox_selector)?;
        let sandbox = registry
            .sandboxes
            .get_mut(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_id))?;

        let workspace_id = resolve_workspace_id(sandbox, workspace_selector)?;
        let current = sandbox
            .workspaces
            .get(&workspace_id)
            .cloned()
            .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_id))?;
        if let Some(pid) = current.runtime_pid {
            session::stop_session(pid, current.runtime_starttime_ticks)?;
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

        let active_process_count = if workspace.status == WorkspaceStatus::Running {
            if let Some(pid) = workspace.runtime_pid {
                if session::process_matches(pid, workspace.runtime_starttime_ticks) {
                    session::count_processes_in_pid_namespace(pid).unwrap_or(0)
                } else {
                    0
                }
            } else {
                0
            }
        } else {
            0
        };
        let resource_usage = if workspace.status == WorkspaceStatus::Running {
            if let Some(pid) = workspace.runtime_pid {
                if session::process_matches(pid, workspace.runtime_starttime_ticks) {
                    session::process_resource_usage(pid).ok()
                } else {
                    None
                }
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
            status: workspace.status.clone(),
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
    let workspace = sandbox
        .workspaces
        .get_mut(workspace_id)
        .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_id))?;

    if let Some(pid) = workspace.runtime_pid {
        remove_workspace_cgroups(&sandbox.metadata, pid);
    }

    if let Some(ref ip) = workspace.assigned_ip {
        network::teardown_workspace_network(ip);
    }

    workspace.status = WorkspaceStatus::Stopped;
    workspace.runtime_pid = None;
    workspace.runtime_starttime_ticks = None;
    workspace.assigned_ip = None;
    normalize_namespace_ref_paths(workspace);
    if let Err(err) = session::write_namespace_ref_values(workspace, "unassigned", "unassigned") {
        tracing::warn!(
            "failed to clear namespace refs for workspace {}: {err:#}",
            workspace.id
        );
    }
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
    if sandbox
        .workspaces
        .values()
        .all(|item| item.status != WorkspaceStatus::Running)
        && !sandbox.metadata.limits.has_limits()
    {
        remove_sandbox_cgroup(&sandbox.metadata);
    }
    Ok(())
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

    if let Some(pid) = workspace.runtime_pid {
        if session::process_matches(pid, workspace.runtime_starttime_ticks) {
            apply_workspace_runtime_constraints(&sandbox, &workspace, pid)?;
        }
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
        .all(|workspace| workspace.status != WorkspaceStatus::Running)
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
        let Some(pid) = workspace.runtime_pid else {
            continue;
        };
        if !session::process_matches(pid, workspace.runtime_starttime_ticks) {
            continue;
        }
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

fn validated_workspace_dir(
    sandbox: &RegistrySandbox,
    workspace: &WorkspaceMetadata,
) -> Result<PathBuf> {
    let sandbox_base = PathBuf::from(&sandbox.metadata.sandbox_path);
    let workspace_root = PathBuf::from(&sandbox.metadata.workspaces_path);
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
        .filter(|ws| ws.status == WorkspaceStatus::Running)
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
