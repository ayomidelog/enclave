use super::*;

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
        if current.status.is_transitional() {
            bail!(
                "workspace '{}' is {}; wait for the current operation to finish before resizing",
                current.name,
                current.status.as_str()
            );
        }
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

        let was_running = current.status.is_running();
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

        let resize = crate::workspace::storage::increase_workspace_disk_allocation(
            &current,
            new_disk_bytes,
        )?;
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
