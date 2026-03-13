use super::{
    launch_userns_args, session_helper_path, setgroups_args,
    userns::{IdMapRange, UserNamespaceMode, UserNamespacePlan},
};

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
        env_tokens: Vec::new(),
        published_ports: Vec::new(),
        status: Default::default(),
        runtime_pid: None,
        runtime_starttime_ticks: None,
        namespace_refs: Default::default(),
        limits: Default::default(),
        assigned_ip: None,
    }
}

#[test]
fn setgroups_args_only_denies_setgroups_for_multi_id_mappings() {
    let subordinate = super::userns::UserNamespacePlan {
        owner: "runner".to_string(),
        uid_map: IdMapRange {
            inner_start: 0,
            outer_start: 100000,
            count: 65536,
        },
        gid_map: IdMapRange {
            inner_start: 0,
            outer_start: 100000,
            count: 65536,
        },
    };
    assert_eq!(setgroups_args(&subordinate), vec!["--deny-setgroups"]);

    let identity = super::userns::UserNamespacePlan {
        owner: "root".to_string(),
        uid_map: IdMapRange {
            inner_start: 0,
            outer_start: 0,
            count: 1,
        },
        gid_map: IdMapRange {
            inner_start: 0,
            outer_start: 0,
            count: 1,
        },
    };
    assert!(setgroups_args(&identity).is_empty());
}

#[test]
fn launch_userns_args_enable_mapping_and_setgroups_when_userns_is_enabled() {
    let plan = UserNamespacePlan {
        owner: "alice".to_string(),
        uid_map: IdMapRange {
            inner_start: 0,
            outer_start: 100_000,
            count: 65_536,
        },
        gid_map: IdMapRange {
            inner_start: 0,
            outer_start: 100_000,
            count: 65_536,
        },
    };
    let args = launch_userns_args(&UserNamespaceMode::Enabled(plan));
    assert!(args.contains(&"--enable-userns".to_string()));
    assert!(args.contains(&"--deny-setgroups".to_string()));
    assert!(args.contains(&"100000".to_string()));
    assert!(args.contains(&"65536".to_string()));
}

#[test]
fn launch_userns_args_omit_enable_flag_when_userns_is_disabled() {
    let args = launch_userns_args(&UserNamespaceMode::Disabled);
    assert!(!args.contains(&"--enable-userns".to_string()));
    assert!(!args.contains(&"--deny-setgroups".to_string()));
    assert!(args.contains(&"1".to_string()));
}

#[test]
fn session_helper_path_uses_procfs_exe_reference() {
    let workspace = workspace_fixture();
    assert_eq!(
        session_helper_path(&workspace),
        std::path::PathBuf::from("/tmp/enclave-test/workspaces/ws-123/runtime/session-helper")
    );
}
