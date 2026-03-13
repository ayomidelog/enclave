use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use chrono::{SecondsFormat, Utc};
use uuid::Uuid;

use crate::registry::with_registry_mut;
use crate::sandbox::{effective_rootfs_path, resolve_sandbox_id, SandboxStatus};

use super::ports::PublishedPortSpec;
use super::types::{NamespaceRefs, WorkspaceLimits, WorkspaceMetadata, WorkspaceStatus};

#[derive(Debug, Clone, Default)]
pub struct WorkspaceCreateOptions {
    pub limits: WorkspaceLimits,
    pub home_mount_source: Option<String>,
    pub auth_providers: Vec<String>,
    pub env_tokens: Vec<String>,
    pub published_ports: Vec<PublishedPortSpec>,
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
        env_tokens,
        published_ports,
    } = options;
    limits.validate()?;
    let auth_providers = normalize_auth_providers(auth_providers)?;
    let env_tokens = normalize_env_tokens(env_tokens)?;
    crate::workspace::validate_published_ports(&published_ports)?;

    with_registry_mut(state_dir, |registry| {
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

        for existing in sandbox_entry.workspaces.values() {
            if existing.name == name {
                bail!(
                    "workspace name '{}' already exists in sandbox '{}' (id: {}); \
                     choose a different name or use the existing workspace",
                    name,
                    sandbox_entry.metadata.id,
                    existing.id
                );
            }
        }

        let sandbox = sandbox_entry.metadata.clone();
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
            fs::create_dir(&workspace_dir)
                .with_context(|| format!("failed to create {}", workspace_dir.display()))?;
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
            auth_providers,
            env_tokens,
            published_ports,
            status: WorkspaceStatus::Stopped,
            runtime_pid: None,
            runtime_starttime_ticks: None,
            namespace_refs: NamespaceRefs {
                mount: mount_ns_ref.to_string_lossy().to_string(),
                pid: pid_ns_ref.to_string_lossy().to_string(),
            },
            limits,
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

        sandbox_entry
            .workspaces
            .insert(workspace_id.clone(), metadata.clone());
        Ok(metadata)
    })
}

fn resolve_home_mount_source(home_mount_source: Option<&str>) -> Result<Option<String>> {
    let Some(raw) = home_mount_source else {
        return Ok(None);
    };

    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!("workspace host path (workspace.<id>.path / daemon 'path') must not be empty");
    }

    let host_path = Path::new(trimmed);
    if !host_path.is_absolute() {
        bail!("workspace host path (workspace.<id>.path / daemon 'path') must be absolute");
    }
    if !host_path.is_dir() {
        bail!(
            "workspace host path (workspace.<id>.path / daemon 'path') '{}' must be an existing directory",
            trimmed
        );
    }

    let canonical = host_path.canonicalize().with_context(|| {
        format!(
            "failed to resolve workspace host path (workspace.<id>.path / daemon 'path') '{}'",
            trimmed
        )
    })?;
    Ok(Some(canonical.to_string_lossy().to_string()))
}

pub(crate) fn ensure_traversable_directory_permissions(path: &Path) -> Result<()> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("failed to stat directory {}", path.display()))?;
    if !metadata.is_dir() {
        bail!("expected directory at {}", path.display());
    }

    let current_mode = metadata.permissions().mode();
    let mut perms = metadata.permissions();

    perms.set_mode((current_mode & !0o777) | 0o755);
    fs::set_permissions(path, perms).with_context(|| {
        format!(
            "failed to set directory permissions to 0755 for {}",
            path.display()
        )
    })?;
    Ok(())
}

pub(crate) fn ensure_home_base_skeleton(home_base_path: &Path) -> Result<()> {
    fs::create_dir_all(home_base_path)
        .with_context(|| format!("failed to create home base {}", home_base_path.display()))?;
    let readme = home_base_path.join("README");
    if !readme.exists() {
        fs::write(
            &readme,
            "Shared home base layer for all workspaces in this sandbox.\n",
        )
        .with_context(|| format!("failed to write {}", readme.display()))?;
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 63 {
        bail!("workspace name must be 1-63 characters");
    }

    let mut chars = name.chars();
    let first = chars
        .next()
        .ok_or_else(|| anyhow!("invalid workspace name"))?;
    if !first.is_ascii_alphanumeric() {
        bail!("workspace name must start with an ASCII letter or digit");
    }

    for c in chars {
        if !(c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            bail!("workspace name contains invalid character '{}'", c);
        }
    }
    Ok(())
}

pub(crate) fn normalize_auth_providers(auth_providers: Vec<String>) -> Result<Vec<String>> {
    let mut normalized = std::collections::BTreeSet::new();
    for provider in auth_providers {
        let provider = provider.trim().to_ascii_lowercase();
        if provider.is_empty() {
            continue;
        }
        crate::auth::provider_env_var(&provider)
            .ok_or_else(|| anyhow!("unsupported auth provider '{}'", provider))?;
        normalized.insert(provider);
    }
    Ok(normalized.into_iter().collect())
}

pub(crate) fn normalize_env_tokens(env_tokens: Vec<String>) -> Result<Vec<String>> {
    let mut normalized = std::collections::BTreeSet::new();
    for env_token in env_tokens {
        let env_token = env_token.trim().to_ascii_uppercase();
        if env_token.is_empty() {
            continue;
        }
        crate::auth::provider_for_env_var(&env_token)
            .ok_or_else(|| anyhow!("unsupported environment token '{}'", env_token))?;
        normalized.insert(env_token);
    }
    Ok(normalized.into_iter().collect())
}

fn generate_workspace_id(name: &str) -> String {
    let slug = crate::fsutil::slugify(name, "workspace");
    let random = Uuid::new_v4().simple().to_string();
    format!("{slug}-{}", &random[..12])
}

#[cfg(test)]
#[path = "../../tests/src/workspace/create.rs"]
mod tests;
