//! Tests for workspace storage.
//!
//! The fixture is here because the submodules share it: a workspace record to hang a
//! storage decision on.

use super::*;

fn workspace_fixture() -> super::super::types::WorkspaceMetadata {
    super::super::types::WorkspaceMetadata {
        id: "ws-123".to_string(),
        sandbox_id: "sb-123".to_string(),
        name: "dev".to_string(),
        created_at: "2026-03-11T00:00:00Z".to_string(),
        workspace_path: "/tmp/enclave-test/workspaces/ws-123".to_string(),
        filesystem_path: "/tmp/enclave-test/workspaces/ws-123/fs".to_string(),
        filesystem_mount_target: "/home".to_string(),
        home_mount_source_path: None,
        sandbox_rootfs_path: "/tmp/enclave-test/rootfs".to_string(),
        overlay_home_base_path: "/tmp/enclave-test/home-base".to_string(),
        overlay_home_upper_path: "/tmp/enclave-test/workspaces/ws-123/home-upper".to_string(),
        overlay_home_work_path: "/tmp/enclave-test/workspaces/ws-123/home-work".to_string(),
        overlay_home_merged_path: "/tmp/enclave-test/workspaces/ws-123/home-merged".to_string(),
        auth_providers: Vec::new(),
        owner: None,
        env_tokens: Vec::new(),
        published_ports: Vec::new(),
        status: Default::default(),
        runtime_pid: None,
        runtime_starttime_ticks: None,
        namespace_refs: Default::default(),
        clear_tmp_on_restart: false,
        limits: Default::default(),
        assigned_ip: None,
    }
}

mod limits;
mod resize;
mod tmp;
mod unmount;
