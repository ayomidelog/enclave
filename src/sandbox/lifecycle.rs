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
    restore_shared_rootfs_mounts(state_dir)?;
    reconcile_sandbox_states(state_dir)?;
    reconcile_workspace_states(state_dir)?;
    Ok(())
}

/// Remount shared-base rootfs overlays that are missing.
///
/// The overlay is kernel mount state, so it survives a daemon restart but not
/// a host reboot. Without this, a sandbox created before a reboot would look
/// like it had an empty rootfs.
fn restore_shared_rootfs_mounts(state_dir: &Path) -> Result<()> {
    let sandboxes = with_registry(state_dir, |registry| {
        Ok(registry
            .sandboxes
            .values()
            .filter(|entry| entry.metadata.rootfs_lower_path.is_some())
            .map(|entry| {
                let mut metadata = entry.metadata.clone();
                normalize_sandbox_metadata(&mut metadata);
                metadata
            })
            .collect::<Vec<_>>())
    })?;
    for metadata in sandboxes {
        if let Err(err) = mounts::ensure_rootfs_overlay_mounted(&metadata) {
            tracing::warn!(
                "sandbox '{}': failed to restore shared rootfs base: {err:#}",
                metadata.id
            );
        }
    }
    Ok(())
}

/// Roll back sandboxes left in a transitional state by an interrupted start or
/// stop. Both transitions end in `stopped`, and any rootfs bind mount from the
/// interrupted operation is removed first.
fn reconcile_sandbox_states(state_dir: &Path) -> Result<()> {
    let transitional = with_registry(state_dir, |registry| {
        Ok(registry
            .sandboxes
            .values()
            .filter(|entry| entry.metadata.status.is_transitional())
            .map(|entry| {
                let mut metadata = entry.metadata.clone();
                normalize_sandbox_metadata(&mut metadata);
                metadata
            })
            .collect::<Vec<_>>())
    })?;

    for metadata in transitional {
        let interrupted = metadata.status.clone();
        if let Err(err) = mounts::ensure_rootfs_unmounted(&metadata) {
            tracing::warn!(
                "reconcile: failed to unmount rootfs for interrupted {:?} sandbox '{}': {err:#}",
                interrupted,
                metadata.id
            );
            continue;
        }
        with_registry_mut(state_dir, |registry| {
            let Some(entry) = registry.sandboxes.get_mut(&metadata.id) else {
                return Ok(());
            };
            entry.metadata.status = SandboxStatus::Stopped;
            persist_sandbox_metadata(&entry.metadata)
        })?;
        tracing::warn!(
            "reconcile: rolled back interrupted {:?} transition for sandbox '{}'",
            interrupted,
            metadata.id
        );
    }
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

mod create;
mod destroy;
mod query;
mod setup;
mod start;
mod stop;
mod update;

pub use create::{create_sandbox, create_sandbox_with_options, SandboxCreateOptions};
pub use destroy::destroy_sandbox;
pub use query::{list_sandbox_items, sandbox_status};
pub use setup::exec_setup_command;
pub use start::start_sandbox;
pub use stop::{pause_sandbox, resume_sandbox, stop_sandbox};
pub use update::update_sandbox_limits;

// Shared between the sandbox lifecycle modules above.
pub(crate) use update::persist_sandbox_metadata;

#[cfg(test)]
#[path = "../../tests/src/sandbox/lifecycle.rs"]
mod tests;
