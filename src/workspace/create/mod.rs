//! Creating a workspace.
//!
//! Creation is three steps: validating the definition, preparing the host
//! state the workspace will own, and committing the record. The definition
//! rules live in the definition module, the host-side preparation in the
//! home module, and this one holds the entry points and the order they run
//! in.

mod definition;
mod home;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use chrono::{SecondsFormat, Utc};
use uuid::Uuid;

use crate::registry::{with_registry, with_registry_mut};
use crate::sandbox::{effective_rootfs_path, resolve_sandbox_id, SandboxStatus};

use super::ports::PublishedPortSpec;
use super::types::{NamespaceRefs, WorkspaceLimits, WorkspaceMetadata, WorkspaceStatus};

use definition::generate_workspace_id;
use home::{ensure_home_base_skeleton, resolve_home_mount_source};

// Reached by the workspace session and update paths, and by the create tests.
pub(crate) use definition::{
    normalize_auth_providers, normalize_env_tokens, normalize_owner, validate_name,
};
pub(crate) use home::ensure_traversable_directory_permissions;

/// The disk the sandbox's workspaces have already been promised.
///
/// Every workspace with a managed allocation holds it whether or not its runtime is
/// running, because the image file exists at that size on the host. A sandbox budget
/// is measured against this, so a stopped workspace still counts.
pub(crate) fn sandbox_workspace_disk_bytes(sandbox: &crate::registry::RegistrySandbox) -> u64 {
    sandbox
        .workspaces
        .values()
        .filter_map(|workspace| workspace.limits.disk_bytes)
        .fold(0u64, u64::saturating_add)
}

#[derive(Debug, Clone, Default)]
pub struct WorkspaceCreateOptions {
    pub limits: WorkspaceLimits,
    pub home_mount_source: Option<String>,
    pub auth_providers: Vec<String>,
    pub owner: Option<String>,
    pub env_tokens: Vec<String>,
    pub published_ports: Vec<PublishedPortSpec>,
    pub clear_tmp_on_restart: bool,
}

pub fn create_workspace(
    state_dir: &Path,
    sandbox_selector: &str,
    name: &str,
    limits: WorkspaceLimits,
) -> Result<WorkspaceMetadata> {
    create_workspace_with_options(
        state_dir,
        sandbox_selector,
        name,
        WorkspaceCreateOptions {
            limits,
            ..WorkspaceCreateOptions::default()
        },
    )
}

pub fn create_workspace_with_options(
    state_dir: &Path,
    sandbox_selector: &str,
    name: &str,
    options: WorkspaceCreateOptions,
) -> Result<WorkspaceMetadata> {
    validate_name(name)?;
    let WorkspaceCreateOptions {
        limits,
        home_mount_source,
        auth_providers,
        owner,
        env_tokens,
        published_ports,
        clear_tmp_on_restart,
    } = options;
    limits.validate()?;
    let auth_providers = normalize_auth_providers(auth_providers)?;
    let owner = normalize_owner(owner)?;
    let env_tokens = normalize_env_tokens(env_tokens)?;
    crate::workspace::validate_published_ports(&published_ports)?;
    crate::workspace::validate_workspace_storage_limits(
        home_mount_source.as_deref(),
        limits.disk_bytes,
    )?;

    let sandbox = with_registry(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, sandbox_selector)?;
        let sandbox_entry = registry
            .sandboxes
            .get(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_id))?;

        if sandbox_entry.metadata.status != SandboxStatus::Running {
            bail!(
                "sandbox '{}' is stopped; start it before creating workspaces",
                sandbox_entry.metadata.id
            );
        }

        for existing in sandbox_entry.workspaces.values() {
            if existing.name == name {
                // Coded rather than plain: a name collision is a conflict, and the
                // caller that turns it into a suggestion has to be able to tell it
                // apart from a create that failed for another reason after it had
                // already recorded the workspace.
                return Err(crate::error::coded(
                    crate::error::ErrorCode::Conflict,
                    format!(
                        "workspace name '{}' already exists in sandbox '{}' (id: {}); \
                         choose a different name or use the existing workspace",
                        name, sandbox_entry.metadata.id, existing.id
                    ),
                ));
            }
        }

        // A sandbox disk budget is enforced here, where the allocation is granted,
        // rather than checked later: a workspace that is created and only then found
        // to be over budget would have to be deleted to fix it.
        sandbox_entry.metadata.limits.check_disk_budget(
            sandbox_workspace_disk_bytes(sandbox_entry),
            limits.disk_bytes.unwrap_or(0),
        )?;

        Ok(sandbox_entry.metadata.clone())
    })?;

    let workspace_id = generate_workspace_id(name);
    let sandbox_dir = PathBuf::from(&sandbox.sandbox_path);
    let workspaces_root = crate::fsutil::ensure_path_within(
        &sandbox_dir,
        Path::new(&sandbox.workspaces_path),
        "sandbox workspaces root",
    )?;
    let workspace_dir = crate::fsutil::ensure_path_within(
        &workspaces_root,
        &workspaces_root.join(&workspace_id),
        "workspace directory",
    )?;
    let filesystem_dir = workspace_dir.join("fs");
    let overlay_upper = workspace_dir.join("home-upper");
    let overlay_work = workspace_dir.join("home-work");
    let overlay_merged = workspace_dir.join("home-merged");
    let namespaces_dir = workspace_dir.join("ns");
    let mount_ns_ref = namespaces_dir.join("mnt.ref");
    let pid_ns_ref = namespaces_dir.join("pid.ref");

    let create_result = (|| {
        // Claim the directory before filling it. `workspace.json` is written a few
        // steps later, and until it exists the directory looks like a leftover to
        // a concurrent repair, which would delete it mid-create. The claim has to
        // be in place at the instant the name becomes visible, which is what
        // `create_claimed_directory` guarantees by renaming a marked directory
        // into place.
        crate::fsutil::create_claimed_directory(state_dir, "workspace", &workspace_dir)?;
        fs::create_dir(&filesystem_dir)
            .with_context(|| format!("failed to create {}", filesystem_dir.display()))?;
        ensure_traversable_directory_permissions(&filesystem_dir)?;
        fs::create_dir(&overlay_upper)
            .with_context(|| format!("failed to create {}", overlay_upper.display()))?;
        fs::create_dir(&overlay_work)
            .with_context(|| format!("failed to create {}", overlay_work.display()))?;
        fs::create_dir(&overlay_merged)
            .with_context(|| format!("failed to create {}", overlay_merged.display()))?;
        fs::create_dir(&namespaces_dir)
            .with_context(|| format!("failed to create {}", namespaces_dir.display()))?;

        crate::fsutil::write_file_atomic(&mount_ns_ref, b"unassigned\n", 0o600)
            .with_context(|| format!("failed to write {}", mount_ns_ref.display()))?;
        crate::fsutil::write_file_atomic(&pid_ns_ref, b"unassigned\n", 0o600)
            .with_context(|| format!("failed to write {}", pid_ns_ref.display()))?;
        let home_base = crate::fsutil::ensure_path_within(
            &sandbox_dir,
            Path::new(&sandbox.home_base_path),
            "sandbox home base",
        )?;
        ensure_home_base_skeleton(&home_base)?;
        Ok::<(), anyhow::Error>(())
    })();
    if let Err(err) = create_result {
        if workspace_dir.exists() {
            fs::remove_dir_all(&workspace_dir).with_context(|| {
                format!(
                    "failed to clean up partial workspace at {}",
                    workspace_dir.display()
                )
            })?;
        }
        return Err(err);
    }

    let preparation = (|| {
        let home_mount_source_path = resolve_home_mount_source(home_mount_source.as_deref())?;
        let metadata = WorkspaceMetadata {
            id: workspace_id.clone(),
            sandbox_id: sandbox.id.clone(),
            name: name.to_string(),
            created_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
            workspace_path: workspace_dir.to_string_lossy().to_string(),
            filesystem_path: filesystem_dir.to_string_lossy().to_string(),
            filesystem_mount_target: "/home".to_string(),
            home_mount_source_path,
            sandbox_rootfs_path: effective_rootfs_path(&sandbox),
            overlay_home_base_path: sandbox.home_base_path.clone(),
            overlay_home_upper_path: overlay_upper.to_string_lossy().to_string(),
            overlay_home_work_path: overlay_work.to_string_lossy().to_string(),
            overlay_home_merged_path: overlay_merged.to_string_lossy().to_string(),
            auth_providers: auth_providers.clone(),
            owner: owner.clone(),
            env_tokens: env_tokens.clone(),
            published_ports: published_ports.clone(),
            status: WorkspaceStatus::Stopped,
            runtime_pid: None,
            runtime_starttime_ticks: None,
            namespace_refs: NamespaceRefs {
                mount: mount_ns_ref.to_string_lossy().to_string(),
                pid: pid_ns_ref.to_string_lossy().to_string(),
            },
            limits: limits.clone(),
            clear_tmp_on_restart,
            assigned_ip: None,
        };

        let metadata_path = workspace_dir.join("workspace.json");
        let metadata_raw = serde_json::to_string_pretty(&metadata)?;
        crate::fsutil::write_file_atomic(&metadata_path, metadata_raw.as_bytes(), 0o600)
            .with_context(|| {
                format!(
                    "failed to write workspace metadata {}",
                    metadata_path.display()
                )
            })?;
        if let Err(err) = crate::workspace::create_workspace_storage(&metadata)
            .and_then(|()| crate::workspace::ensure_workspace_storage_ready(&metadata))
        {
            let _ = crate::workspace::ensure_workspace_storage_unmounted(&metadata);
            return Err(err).with_context(|| {
                format!(
                    "failed to initialize workspace storage {}",
                    workspace_dir.display()
                )
            });
        }
        Ok::<WorkspaceMetadata, anyhow::Error>(metadata)
    })();
    let metadata = match preparation {
        Ok(metadata) => metadata,
        Err(error) => {
            let _ = fs::remove_dir_all(&workspace_dir);
            return Err(error);
        }
    };

    let commit_result = with_registry_mut(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, sandbox_selector)?;
        let sandbox_entry = registry
            .sandboxes
            .get_mut(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_id))?;
        if sandbox_entry.metadata.status != SandboxStatus::Running {
            bail!(
                "sandbox '{}' is stopped; start it before creating workspaces",
                sandbox_entry.metadata.id
            );
        }
        if sandbox_entry
            .workspaces
            .values()
            .any(|existing| existing.name == name)
        {
            return Err(crate::error::coded(
                crate::error::ErrorCode::Conflict,
                format!(
                    "workspace name '{}' already exists in sandbox '{}'",
                    name, sandbox_entry.metadata.id
                ),
            ));
        }
        sandbox_entry
            .workspaces
            .insert(workspace_id.clone(), metadata.clone());
        Ok(metadata.clone())
    });
    if let Err(err) = commit_result {
        let _ = crate::workspace::ensure_workspace_storage_unmounted(&metadata);
        let _ = fs::remove_dir_all(&workspace_dir);
        return Err(err).context("failed to commit workspace metadata");
    }
    crate::fsutil::remove_creation_marker(&workspace_dir);
    commit_result
}

#[cfg(test)]
#[path = "../../../tests/src/workspace/create.rs"]
mod tests;
