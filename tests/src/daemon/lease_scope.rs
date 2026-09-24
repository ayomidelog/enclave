use std::fs;

use serde_json::json;

use crate::daemon::leases::LeaseScope;

use super::{scope_for, Action};

fn state_dir(label: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "enclave-lease-scope-{}-{label}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    crate::registry::ensure_registry(&dir).unwrap();
    dir
}

#[test]
fn read_only_and_per_command_requests_take_no_lease() {
    let dir = state_dir("readonly");
    let params = json!({"sandbox": "box", "workspace": "ws"});

    for action in [
        Action::Ping,
        Action::SandboxStatus,
        Action::SandboxList,
        Action::WorkspaceStatus,
        Action::WorkspaceList,
        Action::WorkspaceStats,
        Action::WorkspaceLogs,
        Action::WorkspaceRuntime,
        Action::WorkspaceExec,
        Action::WorkspaceCp,
        Action::WorkspacePortList,
        Action::WorkspaceSnapshotList,
        Action::PolicyGet,
    ] {
        assert_eq!(
            scope_for(&dir, action, &params).unwrap(),
            None,
            "{action:?} must not take a lifecycle lease"
        );
    }

    let _ = fs::remove_dir_all(dir);
}

#[test]
fn sandbox_wide_requests_take_the_sandbox_scope() {
    let dir = state_dir("sandbox");
    let params = json!({"sandbox": "box"});

    for action in [
        Action::SandboxStart,
        Action::SandboxStop,
        Action::SandboxDestroy,
        Action::SandboxPause,
        Action::SandboxResume,
        Action::SandboxUpdate,
    ] {
        assert_eq!(
            scope_for(&dir, action, &params).unwrap(),
            Some(LeaseScope::Sandbox("box".to_string())),
            "{action:?} must take the sandbox scope"
        );
    }

    let _ = fs::remove_dir_all(dir);
}

#[test]
fn workspace_requests_take_the_workspace_scope() {
    let dir = state_dir("workspace");
    let params = json!({"sandbox": "box", "workspace": "ws"});

    for action in [
        Action::WorkspaceStart,
        Action::WorkspaceStop,
        Action::WorkspaceDestroy,
        Action::WorkspaceResize,
        Action::WorkspaceUpdate,
        Action::WorkspaceRestore,
        Action::WorkspaceSnapshot,
        Action::WorkspacePortPublish,
    ] {
        assert_eq!(
            scope_for(&dir, action, &params).unwrap(),
            Some(LeaseScope::Workspace {
                sandbox: "box".to_string(),
                workspace: "ws".to_string(),
            }),
            "{action:?} must take the workspace scope"
        );
    }

    let _ = fs::remove_dir_all(dir);
}

#[test]
fn whole_host_requests_take_the_global_scope() {
    let dir = state_dir("global");
    let params = json!({});

    for action in [
        Action::SandboxWipe,
        Action::WorkspaceWipe,
        Action::RegistryRepair,
        Action::DaemonDoctorRepair,
    ] {
        assert_eq!(
            scope_for(&dir, action, &params).unwrap(),
            Some(LeaseScope::Global),
            "{action:?} must take the global scope"
        );
    }

    let _ = fs::remove_dir_all(dir);
}

#[test]
fn a_request_without_a_target_takes_no_lease() {
    let dir = state_dir("untargeted");

    assert_eq!(
        scope_for(&dir, Action::SandboxStop, &json!({})).unwrap(),
        None,
        "the handler reports the missing parameter; there is nothing to serialize"
    );

    let _ = fs::remove_dir_all(dir);
}

#[test]
fn a_named_sandbox_resolves_to_the_same_lease_as_its_id() {
    let dir = state_dir("resolved");
    let mut sandbox = crate::sandbox::SandboxMetadata {
        id: "box-abc123".to_string(),
        name: "box".to_string(),
        suite: "bookworm".to_string(),
        mirror: "https://deb.debian.org/debian".to_string(),
        bootstrap_method: crate::sandbox::BootstrapMethod::CachedRootfs,
        created_at: "2026-08-06T00:00:00Z".to_string(),
        sandbox_path: dir
            .join("sandboxes/box-abc123")
            .to_string_lossy()
            .to_string(),
        rootfs_path: dir
            .join("sandboxes/box-abc123/rootfs")
            .to_string_lossy()
            .to_string(),
        rootfs_lower_path: None,
        mounted_rootfs_path: dir
            .join("sandboxes/box-abc123/runtime/rootfs.mnt")
            .to_string_lossy()
            .to_string(),
        workspaces_path: dir
            .join("sandboxes/box-abc123/workspaces")
            .to_string_lossy()
            .to_string(),
        home_base_path: dir
            .join("sandboxes/box-abc123/home-base")
            .to_string_lossy()
            .to_string(),
        limits: crate::sandbox::SandboxLimits::default(),
        status: crate::sandbox::SandboxStatus::Running,
    };
    crate::sandbox::normalize_sandbox_metadata(&mut sandbox);
    crate::registry::with_registry_mut(&dir, |registry| {
        registry.sandboxes.insert(
            sandbox.id.clone(),
            crate::registry::RegistrySandbox {
                metadata: sandbox.clone(),
                workspaces: std::collections::BTreeMap::new(),
            },
        );
        Ok(())
    })
    .unwrap();

    let by_id = scope_for(&dir, Action::SandboxStop, &json!({"sandbox": "box-abc123"})).unwrap();
    let by_name = scope_for(&dir, Action::SandboxStop, &json!({"sandbox": "box"})).unwrap();
    assert_eq!(by_id, by_name);
    assert_eq!(by_id, Some(LeaseScope::Sandbox("box-abc123".to_string())));

    let _ = fs::remove_dir_all(dir);
}
