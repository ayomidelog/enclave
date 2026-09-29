use super::*;

/// The parts of a workspace definition a caller may change.
///
/// Every field is optional and means "leave this alone", which is what lets one
/// request change the limits without disturbing the declared ports. `owner` has
/// a third state because a binding can be removed as well as set.
pub struct WorkspaceDefinitionUpdate {
    pub auth_providers: Option<Vec<String>>,
    /// `None` leaves the binding alone, `Some(None)` clears it, and
    /// `Some(Some(id))` binds the workspace to that auth namespace.
    pub owner: Option<Option<String>>,
    pub env_tokens: Option<Vec<String>>,
    pub published_ports: Option<Vec<PublishedPortSpec>>,
    pub limits: WorkspaceLimitsUpdate,
}

pub fn update_workspace_definition(
    state_dir: &std::path::Path,
    sandbox_selector: &str,
    workspace_selector: &str,
    update: WorkspaceDefinitionUpdate,
) -> Result<WorkspaceMetadata> {
    let WorkspaceDefinitionUpdate {
        auth_providers,
        owner,
        env_tokens,
        published_ports,
        limits: limits_update,
    } = update;
    let auth_providers = auth_providers
        .map(crate::workspace::create::normalize_auth_providers)
        .transpose()?;
    let owner = owner
        .map(crate::workspace::create::normalize_owner)
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
        if let Some(owner) = owner {
            if workspace.owner != owner {
                workspace.owner = owner;
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

mod resize;

pub use resize::resize_workspace_disk;
pub use resize::resize_workspace_memory;
pub(crate) use resize::resize_workspace_with_security;
