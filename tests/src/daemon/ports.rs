//! Which sandboxes a daemon restart republishes ports for.

use super::*;

use crate::registry::{Registry, RegistrySandbox};
use crate::sandbox::{BootstrapMethod, SandboxLimits, SandboxMetadata, SandboxStatus};

fn sandbox(id: &str, status: SandboxStatus) -> RegistrySandbox {
    RegistrySandbox {
        metadata: SandboxMetadata {
            id: id.to_string(),
            name: id.to_string(),
            suite: "bookworm".to_string(),
            mirror: "https://deb.debian.org/debian".to_string(),
            bootstrap_method: BootstrapMethod::CachedRootfs,
            created_at: "2026-08-06T00:00:00Z".to_string(),
            sandbox_path: format!("/tmp/{id}"),
            rootfs_path: format!("/tmp/{id}/rootfs"),
            rootfs_lower_path: None,
            mounted_rootfs_path: format!("/tmp/{id}/runtime/rootfs.mnt"),
            workspaces_path: format!("/tmp/{id}/workspaces"),
            home_base_path: format!("/tmp/{id}/home-base"),
            limits: SandboxLimits::default(),
            status,
        },
        workspaces: std::collections::BTreeMap::new(),
    }
}

/// A paused sandbox must not have its published ports handed back.
///
/// Pausing freezes the workspaces and withdraws their host listeners, so the
/// operator can bind the ports the sandbox declared. The workspaces keep the
/// running status through the pause, so a republish that looked only at the
/// workspace would take the port back from whoever bound it while the sandbox was
/// paused, and no command would have asked for it.
#[test]
fn a_paused_sandbox_does_not_have_its_ports_republished() {
    let mut registry = Registry::default();
    for (id, status) in [
        ("paused", SandboxStatus::Paused),
        ("running", SandboxStatus::Running),
        ("stopped", SandboxStatus::Stopped),
    ] {
        registry
            .sandboxes
            .insert(id.to_string(), sandbox(id, status));
    }

    let holding = sandboxes_holding_ports(&registry);
    assert_eq!(
        holding,
        vec!["running".to_string()],
        "only a running sandbox can answer on a published port"
    );
}
