//! Tests for the sandbox lifecycle.
//!
//! The fixture is here because the submodules share it: a sandbox record left in a
//! transitional state, which is the shape an interrupted operation leaves behind.

use super::*;
use crate::registry::{with_registry, with_registry_mut, RegistrySandbox};
use crate::sandbox::{BootstrapMethod, SandboxLimits, SandboxStatus};
use crate::workspace::{WorkspaceLimits, WorkspaceMetadata, WorkspaceStatus};
use std::collections::BTreeMap;
use std::fs;

fn transitional_sandbox(
    state_dir: &std::path::Path,
    id: &str,
    status: SandboxStatus,
) -> SandboxMetadata {
    let sandbox_path = state_dir.join("sandboxes").join(id);
    fs::create_dir_all(&sandbox_path).unwrap();
    SandboxMetadata {
        id: id.to_string(),
        name: id.to_string(),
        suite: "bookworm".to_string(),
        mirror: "https://deb.debian.org/debian".to_string(),
        bootstrap_method: BootstrapMethod::CachedRootfs,
        created_at: "2026-08-06T00:00:00Z".to_string(),
        sandbox_path: sandbox_path.to_string_lossy().to_string(),
        rootfs_path: sandbox_path.join("rootfs").to_string_lossy().to_string(),
        rootfs_lower_path: None,
        mounted_rootfs_path: sandbox_path
            .join("runtime")
            .join("rootfs.mnt")
            .to_string_lossy()
            .to_string(),
        workspaces_path: sandbox_path
            .join("workspaces")
            .to_string_lossy()
            .to_string(),
        home_base_path: sandbox_path.join("home-base").to_string_lossy().to_string(),
        limits: SandboxLimits::default(),
        status,
    }
}

mod budget;
mod destroy;
mod reconcile;
