//! Resizing a workspace's disk and memory allocation, and relaunching what was
//! running.
//!
//! The two limits need different work. Memory is a cgroup value, so it is applied to
//! the runtime that is already there and a running workspace is never interrupted for
//! it. Disk is an image and the filesystem inside it, which can only be resized with
//! the workspace stopped, so a disk resize stops the runtime, resizes, and relaunches
//! it.
//!
//! The registry is told the size that is actually on disk before anything is
//! relaunched, so a failure part way through leaves a record that describes the real
//! image rather than the one that was asked for.

use super::*;
use crate::workspace::sync_workspace_runtime_limits;

/// What a resize needs from the registry, read under the lock so the resize
/// itself can run without holding it.
struct ResizePlan {
    sandbox_id: String,
    workspace_id: String,
    sandbox: SandboxMetadata,
    workspace: WorkspaceMetadata,
    /// The managed disk the workspace has, when it has one.
    ///
    /// A workspace is only disk-backed if it was created with `disk_mb`, and memory
    /// is independent of that: a workspace whose `/home` is a host directory still has
    /// a memory limit that can be changed. So a missing allocation is not a reason to
    /// refuse a memory resize, and only a resize that asks to change the disk needs one.
    previous_disk_bytes: Option<u64>,
    /// The disk the sandbox's workspaces hold apart from the one being resized.
    ///
    /// Read with the plan so the budget check and the resize describe the same
    /// moment: a workspace created in between is refused its own allocation under
    /// the same lock, so neither of them can take the sandbox past its budget.
    other_workspace_disk_bytes: u64,
}

/// Resize a workspace's disk, its memory, or both.
///
/// Either argument may be absent, which leaves that limit alone. A value equal to
/// what the workspace already has is not a change and is reported as such.
pub fn resize_workspace_with_security(
    state_dir: &std::path::Path,
    sandbox_selector: &str,
    workspace_selector: &str,
    new_disk_bytes: Option<u64>,
    new_memory_bytes: Option<Option<u64>>,
    apparmor_profile: Option<&str>,
    selinux_label: Option<&str>,
) -> Result<WorkspaceResizeResult> {
    // The registry lock is held only while the registry is read or written.
    // Stopping the runtime, resizing the image, and relaunching it take seconds
    // on a large workspace, and holding the lock across them would stall every
    // other workspace operation in the daemon. Operations on one workspace are
    // serialized by the daemon lease, not by this lock.
    let plan = read_resize_plan(state_dir, sandbox_selector, workspace_selector)?;

    let disk_change = new_disk_bytes.filter(|bytes| Some(*bytes) != plan.previous_disk_bytes);
    let memory_change =
        new_memory_bytes.filter(|bytes| *bytes != plan.workspace.limits.memory_bytes);

    if disk_change.is_none() && memory_change.is_none() {
        return Ok(plan.result(
            plan.previous_disk_bytes,
            plan.workspace.limits.memory_bytes,
            false,
        ));
    }

    // A disk change needs a managed disk to change. This is the one place that is
    // required, and it is required here rather than when the plan is read so a memory
    // resize is not refused for a disk it never mentioned.
    if disk_change.is_some() && plan.previous_disk_bytes.is_none() {
        bail!(
            "workspace '{}' has no Enclave-managed disk allocation; configure disk_mb when creating it",
            plan.workspace.name
        );
    }

    // Every reason this request could be refused is checked here, before anything is
    // stopped or written. A disk resize stops the runtime, and a refusal after that
    // point would leave a workspace down for a request that was never possible.
    //
    // A grow also has to fit the sandbox's disk budget. A shrink always fits: it
    // releases space rather than taking it.
    if let Some(new_disk_bytes) = disk_change {
        if plan
            .previous_disk_bytes
            .is_some_and(|current| new_disk_bytes > current)
        {
            plan.sandbox
                .limits
                .check_disk_budget(plan.other_workspace_disk_bytes, new_disk_bytes)?;
        }
        // This checks the floor, the image, and whether anything still holds it.
        crate::workspace::storage::plan_workspace_disk_resize(&plan.workspace, new_disk_bytes)?;
    }

    // Memory is recorded first. It is cheap, it does not disturb the runtime, and
    // doing it before the disk work means a disk failure leaves the memory limit the
    // caller asked for rather than silently discarding it.
    if let Some(memory_bytes) = memory_change {
        set_workspace_memory_bytes(state_dir, &plan, memory_bytes)?;
    }

    let (new_disk_bytes, restarted) = match disk_change {
        Some(new_disk_bytes) => resize_workspace_image(
            state_dir,
            &plan,
            new_disk_bytes,
            apparmor_profile,
            selinux_label,
        )?,
        None => {
            // No image work, so a running runtime keeps running and only its cgroup
            // is brought up to the new limit.
            sync_workspace_runtime_limits(state_dir, &plan.sandbox_id, &plan.workspace_id)?;
            (plan.previous_disk_bytes, false)
        }
    };

    Ok(plan.result(
        new_disk_bytes,
        memory_change.unwrap_or(plan.workspace.limits.memory_bytes),
        restarted,
    ))
}

/// Stop the workspace if it is running, resize its image, and relaunch it.
///
/// Returns the disk size that is now on disk, which is the one the registry was told
/// about, and whether a running runtime was relaunched for it.
fn resize_workspace_image(
    state_dir: &std::path::Path,
    plan: &ResizePlan,
    new_disk_bytes: u64,
    apparmor_profile: Option<&str>,
    selinux_label: Option<&str>,
) -> Result<(Option<u64>, bool)> {
    let was_running = plan.workspace.status.is_running();

    // Stop the runtime before the image is touched and record the stop, so the
    // registry never claims a running runtime whose image is being resized.
    if was_running {
        stop_workspace_for_resize(state_dir, plan)?;
    }

    crate::workspace::ensure_workspace_storage_unmounted(&plan.workspace).with_context(|| {
        format!(
            "failed to unmount workspace '{}' before resizing",
            plan.workspace.name
        )
    })?;

    let resize = crate::workspace::storage::resize_workspace_disk_allocation(
        &plan.workspace,
        new_disk_bytes,
    )?;

    // Record the size that is actually on disk before relaunching, so a failure
    // during the restart leaves the registry describing the real image.
    set_workspace_disk_bytes(state_dir, plan, resize.new_bytes)?;

    if was_running {
        restart_workspace_after_resize(state_dir, plan, apparmor_profile, selinux_label)?;
    }
    Ok((Some(resize.new_bytes), was_running))
}

impl ResizePlan {
    fn result(
        &self,
        new_disk_bytes: Option<u64>,
        new_memory_bytes: Option<u64>,
        restarted: bool,
    ) -> WorkspaceResizeResult {
        WorkspaceResizeResult {
            workspace_id: self.workspace_id.clone(),
            workspace_name: self.workspace.name.clone(),
            previous_disk_bytes: self.previous_disk_bytes,
            new_disk_bytes,
            previous_memory_bytes: self.workspace.limits.memory_bytes,
            new_memory_bytes,
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
        let previous_disk_bytes = workspace.limits.disk_bytes;
        if workspace.home_mount_source_path.is_some() {
            bail!(
                "workspace '{}' uses a host-backed workspace directory; disk resize is only supported for Enclave-managed storage",
                workspace.name
            );
        }
        // The budget is measured on what the other workspaces hold, so a resize that
        // replaces this workspace's allocation is not charged for both.
        let other_workspace_disk_bytes = sandbox
            .workspaces
            .values()
            .filter(|other| other.id != workspace_id)
            .filter_map(|other| other.limits.disk_bytes)
            .fold(0u64, u64::saturating_add);
        Ok(ResizePlan {
            sandbox_id,
            workspace_id,
            sandbox: sandbox.metadata.clone(),
            workspace,
            previous_disk_bytes,
            other_workspace_disk_bytes,
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

fn set_workspace_memory_bytes(
    state_dir: &std::path::Path,
    plan: &ResizePlan,
    new_memory_bytes: Option<u64>,
) -> Result<()> {
    with_registry_mut(state_dir, |registry| {
        let workspace = registry
            .sandboxes
            .get_mut(&plan.sandbox_id)
            .and_then(|sandbox| sandbox.workspaces.get_mut(&plan.workspace_id))
            .ok_or_else(|| anyhow!("workspace '{}' not found", plan.workspace_id))?;
        workspace.limits.memory_bytes = new_memory_bytes;
        workspace.limits.validate()?;
        persist_workspace_metadata(workspace)
    })
}

fn restart_workspace_after_resize(
    state_dir: &std::path::Path,
    plan: &ResizePlan,
    apparmor_profile: Option<&str>,
    selinux_label: Option<&str>,
) -> Result<bool> {
    // The relaunch reads the record rather than the plan, so the memory limit that
    // was just written is the one the new runtime is started with.
    let workspace = with_registry(state_dir, |registry| {
        registry
            .sandboxes
            .get(&plan.sandbox_id)
            .and_then(|sandbox| sandbox.workspaces.get(&plan.workspace_id))
            .cloned()
            .ok_or_else(|| anyhow!("workspace '{}' not found", plan.workspace_id))
    })?;
    // Reserving the address durably marks the workspace as starting, so a crash
    // during the relaunch is visible instead of looking like a stopped workspace.
    let reserved_ip = mark_workspace_starting(state_dir, &plan.sandbox_id, &plan.workspace_id)?;
    let started = match launch_workspace_runtime(
        state_dir,
        &plan.sandbox,
        &workspace,
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

/// Resize a workspace's disk allocation.
pub fn resize_workspace_disk(
    state_dir: &std::path::Path,
    sandbox_selector: &str,
    workspace_selector: &str,
    new_disk_bytes: u64,
) -> Result<WorkspaceResizeResult> {
    resize_workspace_with_security(
        state_dir,
        sandbox_selector,
        workspace_selector,
        Some(new_disk_bytes),
        None,
        None,
        None,
    )
}

/// Resize a workspace's memory limit, where `None` removes the limit.
///
/// A workspace whose storage Enclave does not manage has no disk to resize, but it
/// still has a memory limit, so this is usable on every workspace.
pub fn resize_workspace_memory(
    state_dir: &std::path::Path,
    sandbox_selector: &str,
    workspace_selector: &str,
    new_memory_bytes: Option<u64>,
) -> Result<WorkspaceResizeResult> {
    resize_workspace_with_security(
        state_dir,
        sandbox_selector,
        workspace_selector,
        None,
        Some(new_memory_bytes),
        None,
        None,
    )
}
