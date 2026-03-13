use super::runtime_exec_command_args;
use crate::workspace::{WorkspaceLimits, WorkspaceMetadata, WorkspaceStatus};

#[test]
fn runtime_exec_clears_environment_before_running_wrapper() {
    let workspace = WorkspaceMetadata {
        id: "ws-1".to_string(),
        sandbox_id: "sb-1".to_string(),
        name: "workspace".to_string(),
        created_at: "2026-01-01T00:00:00Z".to_string(),
        workspace_path: "/tmp/ws".to_string(),
        filesystem_path: "/tmp/ws/fs".to_string(),
        filesystem_mount_target: "/home".to_string(),
        home_mount_source_path: None,
        sandbox_rootfs_path: "/var/lib/enclave/rootfs".to_string(),
        overlay_home_base_path: "/tmp/base".to_string(),
        overlay_home_upper_path: "/tmp/upper".to_string(),
        overlay_home_work_path: "/tmp/work".to_string(),
        overlay_home_merged_path: "/tmp/merged".to_string(),
        auth_providers: vec![],
        env_tokens: vec![],
        published_ports: vec![],
        status: WorkspaceStatus::Running,
        runtime_pid: Some(1234),
        runtime_starttime_ticks: Some(1),
        namespace_refs: Default::default(),
        limits: WorkspaceLimits::default(),
        assigned_ip: None,
    };
    let args = runtime_exec_command_args(
        1234,
        1,
        &workspace.sandbox_id,
        &workspace.id,
        "/home",
        &["/bin/echo".to_string(), "ok".to_string()],
    );
    assert_eq!(args[0], "internal");
    assert_eq!(args[1], "workspace-command");
    assert_eq!(args[2], "--runtime-pid");
    assert_eq!(args[3], "1234");
    assert_eq!(args[4], "--runtime-starttime-ticks");
    assert_eq!(args[5], "1");
    assert_eq!(args[6], "--cwd");
    assert_eq!(args[7], "/home");
    assert_eq!(args[8], "--sandbox-id");
    assert_eq!(args[9], "sb-1");
    assert_eq!(args[10], "--workspace-id");
    assert_eq!(args[11], "ws-1");
    assert_eq!(args[12], "/bin/echo");
    assert_eq!(args[13], "ok");
}
