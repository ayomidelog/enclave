use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};
use chrono::{SecondsFormat, Utc};

use crate::registry::{
    ensure_registry, repair_registry, with_registry, with_registry_mut, RegistrySandbox,
};

use super::bootstrap;
use super::mounts;
use super::setup_cache;
use super::types::{
    BootstrapMethod, SandboxLimits, SandboxLimitsUpdate, SandboxListItem, SandboxMetadata,
    SandboxStatus, SandboxStatusReport,
};
use super::util::{
    dir_size, ensure_sandbox_layout, generate_sandbox_id, normalize_sandbox_metadata,
    resolve_sandbox_id, sandboxes_dir, validate_debootstrap_inputs, validate_name,
};

pub fn init_storage(state_dir: &Path) -> Result<()> {
    fs::create_dir_all(sandboxes_dir(state_dir))
        .with_context(|| format!("failed to initialize storage at {}", state_dir.display()))?;
    bootstrap::ensure_rootfs_cache(state_dir)?;
    ensure_registry(state_dir)?;
    repair_registry(state_dir, false)?;
    reconcile_workspace_states(state_dir)?;
    Ok(())
}

fn reconcile_workspace_states(state_dir: &Path) -> Result<()> {
    with_registry_mut(state_dir, |registry| {
        let mut reconciled = 0usize;
        for sandbox in registry.sandboxes.values_mut() {
            for workspace in sandbox.workspaces.values_mut() {
                if crate::workspace::reconcile_workspace_runtime_state(workspace)? {
                    tracing::warn!(
                        "reconcile: repaired stale runtime state for workspace '{}'",
                        workspace.id
                    );
                    reconciled += 1;
                }
            }
        }
        if reconciled > 0 {
            tracing::warn!("reconcile: recovered {} stale workspace(s)", reconciled);
        }
        Ok(())
    })
}

pub fn create_sandbox(
    state_dir: &Path,
    debootstrap_binary: &str,
    name: &str,
    suite: &str,
    mirror: &str,
    method: &BootstrapMethod,
) -> Result<SandboxMetadata> {
    create_sandbox_with_options(
        state_dir,
        debootstrap_binary,
        name,
        suite,
        mirror,
        method,
        SandboxCreateOptions::default(),
    )
}

#[derive(Debug, Clone, Default)]
pub struct SandboxCreateOptions {
    pub limits: SandboxLimits,
}

pub fn create_sandbox_with_options(
    state_dir: &Path,
    debootstrap_binary: &str,
    name: &str,
    suite: &str,
    mirror: &str,
    method: &BootstrapMethod,
    options: SandboxCreateOptions,
) -> Result<SandboxMetadata> {
    validate_name(name)?;
    options.limits.validate()?;

    if *method == BootstrapMethod::Debootstrap {
        validate_debootstrap_inputs(suite, mirror)?;
    }

    let euid = unsafe { libc::geteuid() };
    if euid != 0 {
        bail!(
            "sandbox creation requires root privileges (debootstrap must run as root). \
             Re-run with sudo."
        );
    }

    init_storage(state_dir)?;

    let sandbox_id = generate_sandbox_id(name);
    let sandbox_dir = sandboxes_dir(state_dir).join(&sandbox_id);
    let rootfs_dir = sandbox_dir.join("rootfs");
    let runtime_dir = sandbox_dir.join("runtime");
    let mounted_rootfs_dir = runtime_dir.join("rootfs.mnt");
    let workspaces_dir = sandbox_dir.join("workspaces");
    let home_base_dir = sandbox_dir.join("home-base");

    let create_result = (|| {
        fs::create_dir(&sandbox_dir).with_context(|| {
            format!(
                "failed to create sandbox directory {}",
                sandbox_dir.to_string_lossy()
            )
        })?;
        fs::create_dir(&rootfs_dir).with_context(|| {
            format!(
                "failed to create rootfs directory {}",
                rootfs_dir.to_string_lossy()
            )
        })?;
        fs::create_dir(&runtime_dir).with_context(|| {
            format!(
                "failed to create runtime directory {}",
                runtime_dir.to_string_lossy()
            )
        })?;
        fs::create_dir(&mounted_rootfs_dir).with_context(|| {
            format!(
                "failed to create mounted rootfs directory {}",
                mounted_rootfs_dir.to_string_lossy()
            )
        })?;
        fs::create_dir(&workspaces_dir).with_context(|| {
            format!(
                "failed to create workspaces directory {}",
                workspaces_dir.to_string_lossy()
            )
        })?;
        fs::create_dir(&home_base_dir).with_context(|| {
            format!(
                "failed to create home base directory {}",
                home_base_dir.to_string_lossy()
            )
        })?;
        Ok::<(), anyhow::Error>(())
    })();
    if let Err(err) = create_result {
        if sandbox_dir.exists() {
            fs::remove_dir_all(&sandbox_dir).with_context(|| {
                format!(
                    "failed to clean up partial sandbox directory {}",
                    sandbox_dir.display()
                )
            })?;
        }
        return Err(err);
    }

    let bootstrap_result = bootstrap::bootstrap_rootfs(&bootstrap::BootstrapParams {
        method,
        rootfs_dir: &rootfs_dir,
        sandbox_dir: &sandbox_dir,
        debootstrap_binary,
        suite,
        mirror,
        name,
        state_dir,
    });
    if let Err(err) = bootstrap_result {
        if sandbox_dir.exists() {
            fs::remove_dir_all(&sandbox_dir).with_context(|| {
                format!(
                    "failed to clean up sandbox directory after bootstrap failure {}",
                    sandbox_dir.display()
                )
            })?;
        }
        return Err(err);
    }

    let metadata = SandboxMetadata {
        id: sandbox_id,
        name: name.to_string(),
        suite: suite.to_string(),
        mirror: mirror.to_string(),
        bootstrap_method: method.clone(),
        created_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        sandbox_path: sandbox_dir.to_string_lossy().to_string(),
        rootfs_path: rootfs_dir.to_string_lossy().to_string(),
        mounted_rootfs_path: mounted_rootfs_dir.to_string_lossy().to_string(),
        workspaces_path: workspaces_dir.to_string_lossy().to_string(),
        home_base_path: home_base_dir.to_string_lossy().to_string(),
        limits: options.limits,
        status: SandboxStatus::Stopped,
    };

    let metadata_path = sandbox_dir.join("sandbox.json");
    let metadata_json = serde_json::to_string_pretty(&metadata)?;
    crate::fsutil::write_file_atomic(&metadata_path, metadata_json.as_bytes(), 0o600)
        .with_context(|| {
            format!(
                "failed to write sandbox metadata at {}",
                metadata_path.to_string_lossy()
            )
        })?;

    with_registry_mut(state_dir, |registry| {
        registry.sandboxes.insert(
            metadata.id.clone(),
            RegistrySandbox {
                metadata: metadata.clone(),
                workspaces: Default::default(),
            },
        );
        Ok(())
    })?;

    Ok(metadata)
}

pub fn list_sandbox_items(state_dir: &Path) -> Result<Vec<SandboxListItem>> {
    ensure_registry(state_dir)?;
    with_registry(state_dir, |registry| {
        let mut items = Vec::new();
        for entry in registry.sandboxes.values() {
            let mut metadata = entry.metadata.clone();
            normalize_sandbox_metadata(&mut metadata);
            items.push(SandboxListItem {
                id: metadata.id,
                name: metadata.name,
                status: metadata.status,
                workspace_count: entry.workspaces.len(),
            });
        }
        items.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(items)
    })
}

pub fn sandbox_status(state_dir: &Path, selector: &str) -> Result<SandboxStatusReport> {
    with_registry(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, selector)?;
        let entry = registry
            .sandboxes
            .get(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", selector))?;
        let mut metadata = entry.metadata.clone();
        normalize_sandbox_metadata(&mut metadata);

        let usage = dir_size(Path::new(&metadata.rootfs_path))?;
        Ok(SandboxStatusReport {
            id: metadata.id.clone(),
            name: metadata.name.clone(),
            created_at: metadata.created_at.clone(),
            status: metadata.status.clone(),
            rootfs_path: metadata.rootfs_path.clone(),
            rootfs_disk_usage_bytes: usage,
            workspace_count: entry.workspaces.len(),
            limits: metadata.limits.clone(),
        })
    })
}

pub fn update_sandbox_limits(
    state_dir: &Path,
    selector: &str,
    limits: &SandboxLimitsUpdate,
) -> Result<SandboxMetadata> {
    with_registry_mut(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, selector)?;
        let entry = registry
            .sandboxes
            .get_mut(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", selector))?;
        if entry.metadata.limits.apply_update(limits)? {
            persist_sandbox_metadata(&entry.metadata)?;
        }
        Ok(entry.metadata.clone())
    })
}

pub fn start_sandbox(state_dir: &Path, selector: &str) -> Result<SandboxMetadata> {
    let metadata = with_registry_mut(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, selector)?;
        let entry = registry
            .sandboxes
            .get_mut(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", selector))?;
        normalize_sandbox_metadata(&mut entry.metadata);
        ensure_sandbox_layout(&entry.metadata)?;
        mounts::ensure_rootfs_mounted(&entry.metadata)?;
        entry.metadata.status = SandboxStatus::Running;
        persist_sandbox_metadata(&entry.metadata)?;
        Ok(entry.metadata.clone())
    })?;
    crate::workspace::sync_sandbox_runtime_limits(state_dir, &metadata.id)?;
    Ok(metadata)
}

pub fn stop_sandbox(state_dir: &Path, selector: &str) -> Result<SandboxMetadata> {
    let metadata = with_registry_mut(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, selector)?;
        let entry = registry
            .sandboxes
            .get_mut(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", selector))?;
        normalize_sandbox_metadata(&mut entry.metadata);
        mounts::ensure_rootfs_unmounted(&entry.metadata)?;
        entry.metadata.status = SandboxStatus::Stopped;
        persist_sandbox_metadata(&entry.metadata)?;
        Ok(entry.metadata.clone())
    })?;
    let sandbox_cgroup = PathBuf::from("/sys/fs/cgroup")
        .join(crate::sandbox::cgroup::sandbox_cgroup_name(&metadata.id));
    if let Err(err) = crate::sandbox::cgroup::remove_cgroup_path(&sandbox_cgroup) {
        tracing::debug!(
            "sandbox cgroup cleanup skipped for '{}': {err:#}",
            metadata.id
        );
    }
    Ok(metadata)
}

pub fn destroy_sandbox(state_dir: &Path, selector: &str) -> Result<String> {
    let (sandbox_id, sandbox) = with_registry(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, selector)?;
        let sandbox = registry
            .sandboxes
            .get(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", selector))?;

        Ok((sandbox_id, sandbox.clone()))
    })?;

    if !Path::new(&sandbox.metadata.sandbox_path).exists() {
        with_registry_mut(state_dir, |registry| {
            registry.sandboxes.remove(&sandbox_id);
            Ok(())
        })?;
        return Ok(sandbox_id);
    }

    let workspace_ids = sandbox.workspaces.keys().cloned().collect::<Vec<_>>();
    let mut cleanup_errors = Vec::new();
    for workspace_id in workspace_ids {
        if let Err(err) = crate::workspace::destroy_workspace(state_dir, &sandbox_id, &workspace_id)
        {
            cleanup_errors.push(format!("workspace {workspace_id} cleanup: {err:#}"));
        }
    }

    let sandbox_dir = PathBuf::from(&sandbox.metadata.sandbox_path);
    let mut metadata = sandbox.metadata.clone();
    normalize_sandbox_metadata(&mut metadata);
    let rootfs_unmounted = match mounts::ensure_rootfs_unmounted(&metadata) {
        Ok(()) => true,
        Err(error) => {
            cleanup_errors.push(format!("rootfs cleanup: {error:#}"));
            false
        }
    };
    let sandbox_cgroup = PathBuf::from("/sys/fs/cgroup")
        .join(crate::sandbox::cgroup::sandbox_cgroup_name(&metadata.id));
    if let Err(err) = crate::sandbox::cgroup::remove_cgroup_path(&sandbox_cgroup) {
        cleanup_errors.push(format!(
            "cgroup {} cleanup: {err:#}",
            sandbox_cgroup.display()
        ));
    }
    if rootfs_unmounted && cleanup_errors.is_empty() {
        let sandboxes_root = sandboxes_dir(state_dir);
        let sandbox_dir =
            crate::fsutil::ensure_path_within(&sandboxes_root, &sandbox_dir, "sandbox directory")?;
        if sandbox_dir.exists() {
            if let Err(error) = fs::remove_dir_all(&sandbox_dir) {
                cleanup_errors.push(format!(
                    "sandbox directory {} cleanup: {error}",
                    sandbox_dir.display()
                ));
            }
        }

        if sandbox_dir.exists() {
            cleanup_errors.push(format!(
                "sandbox directory {} still exists",
                sandbox_dir.display()
            ));
        }
    }

    if !cleanup_errors.is_empty() {
        bail!(
            "failed to fully destroy sandbox '{}': {}",
            sandbox_id,
            cleanup_errors.join("; ")
        );
    }

    with_registry_mut(state_dir, |registry| {
        let current = registry
            .sandboxes
            .get(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", sandbox_id))?;
        if !current.workspaces.is_empty() {
            bail!(
                "sandbox '{}' still has {} workspace record(s) after cleanup",
                sandbox_id,
                current.workspaces.len()
            );
        }
        registry.sandboxes.remove(&sandbox_id);
        Ok(())
    })?;

    Ok(sandbox_id)
}

pub fn exec_setup_command(
    state_dir: &Path,
    selector: &str,
    command: &str,
    cache_setup: bool,
    setup_digest: Option<&str>,
    setup_index: Option<u64>,
) -> Result<serde_json::Value> {
    use crate::registry::with_registry;

    let rootfs_path = with_registry(state_dir, |registry| {
        let sandbox_id = resolve_sandbox_id(registry, selector)?;
        let entry = registry
            .sandboxes
            .get(&sandbox_id)
            .ok_or_else(|| anyhow!("sandbox '{}' not found", selector))?;
        let mut metadata = entry.metadata.clone();
        normalize_sandbox_metadata(&mut metadata);
        Ok(super::util::effective_rootfs_path(&metadata))
    })?;

    let cache_marker = if cache_setup {
        let digest = setup_digest.ok_or_else(|| anyhow!("cached setup requires setup_digest"))?;
        let index = setup_index.ok_or_else(|| anyhow!("cached setup requires setup_index"))?;
        if setup_cache::is_complete(Path::new(&rootfs_path), digest, index)? {
            return Ok(serde_json::json!({"cached": true, "exit_code": 0}));
        }
        Some((digest.to_string(), index))
    } else {
        None
    };

    let output = Command::new("chroot")
        .arg(&rootfs_path)
        .arg("/bin/sh")
        .arg("-c")
        .arg(command)
        .output()
        .with_context(|| format!("failed to execute setup command in sandbox: {}", command))?;

    let exit_code = output.status.code().unwrap_or(1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if !output.status.success() {
        bail!(
            "setup command exited with status {}: {}{}",
            exit_code,
            stderr.trim(),
            if stderr.is_empty() {
                stdout.trim().to_string()
            } else {
                String::new()
            }
        );
    }

    if let Some((digest, index)) = cache_marker {
        setup_cache::mark_complete(Path::new(&rootfs_path), &digest, index)?;
    }

    Ok(serde_json::json!({
        "exit_code": exit_code,
        "stdout": stdout,
        "stderr": stderr,
    }))
}

fn persist_sandbox_metadata(metadata: &SandboxMetadata) -> Result<()> {
    let metadata_path = PathBuf::from(&metadata.sandbox_path).join("sandbox.json");
    let metadata_json = serde_json::to_string_pretty(metadata)?;
    crate::fsutil::write_file_atomic(&metadata_path, metadata_json.as_bytes(), 0o600).with_context(
        || {
            format!(
                "failed to write sandbox metadata {}",
                metadata_path.display()
            )
        },
    )
}

#[cfg(test)]
#[path = "../../tests/src/sandbox/lifecycle.rs"]
mod tests;
