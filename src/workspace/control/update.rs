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

/// What a resize needs from the registry, read under the lock so the resize
/// itself can run without holding it.
struct ResizePlan {
    sandbox_id: String,
    workspace_id: String,
    sandbox: SandboxMetadata,
    workspace: WorkspaceMetadata,
    previous_disk_bytes: u64,
}

pub fn resize_workspace_disk_with_security(
    state_dir: &std::path::Path,
    sandbox_selector: &str,
    workspace_selector: &str,
    new_disk_bytes: u64,
    apparmor_profile: Option<&str>,
    selinux_label: Option<&str>,
) -> Result<WorkspaceResizeResult> {
    // The registry lock is held only while the registry is read or written.
    // Stopping the runtime, resizing the image, and relaunching it take seconds
    // on a large workspace, and holding the lock across them would stall every
    // other workspace operation in the daemon. Operations on one workspace are
    // serialized by the daemon lease, not by this lock.
    let plan = read_resize_plan(state_dir, sandbox_selector, workspace_selector)?;

    if new_disk_bytes == plan.previous_disk_bytes {
        return Ok(plan.result(plan.previous_disk_bytes, false));
    }

    let was_running = plan.workspace.status.is_running();

    // Stop the runtime before the image is touched and record the stop, so the
    // registry never claims a running runtime whose image is being resized.
    if was_running {
        stop_workspace_for_resize(state_dir, &plan)?;
    }

    crate::workspace::ensure_workspace_storage_unmounted(&plan.workspace).with_context(|| {
        format!(
            "failed to unmount workspace '{}' before resizing",
            plan.workspace.name
        )
    })?;

    let resize = crate::workspace::storage::increase_workspace_disk_allocation(
        &plan.workspace,
        new_disk_bytes,
    )?;

    // Record the size that is actually on disk before relaunching, so a failure
    // during the restart leaves the registry describing the real image.
    set_workspace_disk_bytes(state_dir, &plan, resize.new_bytes)?;

    let restarted = if was_running {
        restart_workspace_after_resize(state_dir, &plan, apparmor_profile, selinux_label)?
    } else {
        false
    };

    Ok(plan.result(resize.new_bytes, restarted))
}

impl ResizePlan {
    fn result(&self, new_disk_bytes: u64, restarted: bool) -> WorkspaceResizeResult {
        WorkspaceResizeResult {
            workspace_id: self.workspace_id.clone(),
            workspace_name: self.workspace.name.clone(),
            previous_disk_bytes: self.previous_disk_bytes,
            new_disk_bytes,
            restarted,
        }
    }
}

fn read_resize_plan(
    state_dir: &std::path::Path,
    sandbox_selector: &str,
    workspace_selector: &str,
) -> Result<ResizePlan> {
    with_registry(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, sandbox_selector)?;
        let sandbox = registry
            .sandboxes
            .get(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_selector))?;
        if sandbox.metadata.status != SandboxStatus::Running {
            bail!(
                "sandbox '{}' is stopped; start it before resizing a workspace",
                sandbox.metadata.id
            );
        }
        let workspace_id = resolve_workspace_id(sandbox, workspace_selector)?;
        let workspace = sandbox
            .workspaces
            .get(&workspace_id)
            .cloned()
            .ok_or_else(|| anyhow!("workspace '{}' not found", workspace_selector))?;
        if workspace.status.is_transitional() {
            bail!(
                "workspace '{}' is {}; wait for the current operation to finish before resizing",
                workspace.name,
                workspace.status.as_str()
            );
        }
        let previous_disk_bytes = workspace.limits.disk_bytes.ok_or_else(|| {
            anyhow!(
                "workspace '{}' has no Enclave-managed disk allocation; configure disk_mb when creating it",
                workspace.name
            )
        })?;
        if workspace.home_mount_source_path.is_some() {
            bail!(
                "workspace '{}' uses a host-backed workspace directory; disk resize is only supported for Enclave-managed storage",
                workspace.name
            );
        }
        Ok(ResizePlan {
            sandbox_id,
            workspace_id,
            sandbox: sandbox.metadata.clone(),
            workspace,
            previous_disk_bytes,
        })
    })
}

fn stop_workspace_for_resize(state_dir: &std::path::Path, plan: &ResizePlan) -> Result<()> {
    mark_workspace_stopping(state_dir, &plan.sandbox_id, &plan.workspace_id)?;
    if let Some(pid) = plan.workspace.runtime_pid {
        session::stop_session(pid, plan.workspace.runtime_starttime_ticks).with_context(|| {
            format!(
                "failed to stop workspace '{}' before resizing",
                plan.workspace.name
            )
        })?;
    }
    with_registry_mut(state_dir, |registry| {
        let sandbox = registry
            .sandboxes
            .get_mut(&plan.sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", plan.sandbox_id))?;
        set_workspace_stopped(sandbox, &plan.workspace_id).map(|_| ())
    })
}

fn set_workspace_disk_bytes(
    state_dir: &std::path::Path,
    plan: &ResizePlan,
    new_disk_bytes: u64,
) -> Result<()> {
    with_registry_mut(state_dir, |registry| {
        let workspace = registry
            .sandboxes
            .get_mut(&plan.sandbox_id)
            .and_then(|sandbox| sandbox.workspaces.get_mut(&plan.workspace_id))
            .ok_or_else(|| anyhow!("workspace '{}' not found", plan.workspace_id))?;
        workspace.limits.disk_bytes = Some(new_disk_bytes);
        persist_workspace_metadata(workspace)
    })
}

fn restart_workspace_after_resize(
    state_dir: &std::path::Path,
    plan: &ResizePlan,
    apparmor_profile: Option<&str>,
    selinux_label: Option<&str>,
) -> Result<bool> {
    // Reserving the address durably marks the workspace as starting, so a crash
    // during the relaunch is visible instead of looking like a stopped workspace.
    let reserved_ip = mark_workspace_starting(state_dir, &plan.sandbox_id, &plan.workspace_id)?;
    let started = match launch_workspace_runtime(
        state_dir,
        &plan.sandbox,
        &plan.workspace,
        apparmor_profile,
        selinux_label,
        &reserved_ip,
    ) {
        Ok(started) => started,
        Err(error) => {
            let _ = mark_workspace_start_failed(state_dir, &plan.sandbox_id, &plan.workspace_id);
            return Err(error);
        }
    };

    let commit = with_registry_mut(state_dir, |registry| {
        let workspace = registry
            .sandboxes
            .get_mut(&plan.sandbox_id)
            .and_then(|sandbox| sandbox.workspaces.get_mut(&plan.workspace_id))
            .ok_or_else(|| anyhow!("workspace '{}' not found", plan.workspace_id))?;
        // `mark_workspace_starting` recorded the transition before the relaunch.
        // Anything else means a competing operation touched the workspace while
        // the launch was in flight, so the captured identity cannot be trusted.
        if workspace.status != WorkspaceStatus::Starting {
            bail!(
                "workspace '{}' is {} while a resize restart was in progress; refusing to commit runtime metadata",
                workspace.id,
                workspace.status.as_str()
            );
        }
        workspace.status = WorkspaceStatus::Running;
        workspace.runtime_pid = Some(started.pid);
        workspace.runtime_starttime_ticks = Some(started.starttime_ticks);
        workspace.assigned_ip = Some(started.assigned_ip.clone());
        normalize_namespace_ref_paths(workspace);
        session::write_namespace_ref_values(workspace, &started.mount_ns, &started.pid_ns)?;
        persist_workspace_metadata(workspace)
    });

    match commit {
        Ok(()) => Ok(true),
        Err(error) => {
            let _ = session::stop_session(started.pid, Some(started.starttime_ticks));
            let _ = mark_workspace_start_failed(state_dir, &plan.sandbox_id, &plan.workspace_id);
            Err(error)
        }
    }
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
