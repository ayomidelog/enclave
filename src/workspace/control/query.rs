use super::*;

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

pub(crate) fn collect_all_used_ip_octets(
    registry: &crate::registry::Registry,
) -> std::collections::BTreeSet<u8> {
    let ips = registry
        .sandboxes
        .values()
        .flat_map(|s| s.workspaces.values())
        .filter_map(|ws| ws.assigned_ip.as_deref());
    // The registry describes this daemon's workspaces, and the host describes every
    // Enclave interface on the machine. A second daemon on this host allocates from
    // its own registry and attaches to the same bridge, so an address only one of
    // the two knows about is an address that can be handed out twice. Unioning the
    // two is what makes the allocator see the other daemon's workspaces.
    let mut used = network::collect_used_ips(ips);
    // An interface on the bridge that names one of this daemon's own workspaces is
    // this daemon's, and a start replaces its own leftover rather than routing around
    // it, so the hash of every workspace here is excluded from the host's answer. A
    // live workspace's address is already in `used` from its record, which is why this
    // only ever frees a leftover.
    let own_hashes = registry
        .sandboxes
        .values()
        .flat_map(|sandbox| sandbox.workspaces.values())
        .map(|workspace| network::veth::workspace_hash(&workspace.id))
        .collect::<std::collections::BTreeSet<_>>();
    used.extend(network::host_veth_octets_held_by_others(&own_hashes));
    used
}

pub fn workspace_runtime_is_active(workspace: &WorkspaceMetadata) -> bool {
    workspace.status == WorkspaceStatus::Running
        && workspace
            .runtime_pid
            .zip(workspace.runtime_starttime_ticks)
            .is_some_and(|(pid, starttime)| {
                session::process_matches(pid, Some(starttime))
                    && session::namespace_refs_match_runtime(workspace, pid)
            })
}
