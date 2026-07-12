use super::{
    infer_workspace_helper_from_current_exe, launch_userns_args, resolve_session_helper_source,
    session_helper_path, setgroups_args, stop_sessions_batch,
    userns::{IdMapRange, UserNamespaceMode, UserNamespacePlan},
};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn workspace_fixture() -> super::super::types::WorkspaceMetadata {
    super::super::types::WorkspaceMetadata {
        id: "ws-123".to_string(),
        sandbox_id: "sb-123".to_string(),
        name: "dev".to_string(),
        created_at: "2026-03-11T00:00:00Z".to_string(),
        workspace_path: "/tmp/enclave-test/sandboxes/sb-123/workspaces/ws-123".to_string(),
        filesystem_path: "/tmp/enclave-test/sandboxes/sb-123/workspaces/ws-123/fs".to_string(),
        filesystem_mount_target: "/home".to_string(),
        home_mount_source_path: None,
        sandbox_rootfs_path: "/tmp/enclave-test/rootfs".to_string(),
        overlay_home_base_path: "/tmp/enclave-test/home-base".to_string(),
        overlay_home_upper_path: "/tmp/enclave-test/sandboxes/sb-123/workspaces/ws-123/home-upper"
            .to_string(),
        overlay_home_work_path: "/tmp/enclave-test/sandboxes/sb-123/workspaces/ws-123/home-work"
            .to_string(),
        overlay_home_merged_path:
            "/tmp/enclave-test/sandboxes/sb-123/workspaces/ws-123/home-merged".to_string(),
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
        std::path::PathBuf::from("/tmp/enclave-test/sandboxes/sb-123/runtime/session-helper")
    );
}

#[test]
fn resolve_session_helper_source_prefers_override_env() {
    let exe = std::env::current_exe().expect("current exe");
    std::env::set_var("ENCLAVE_SELF_EXE", &exe);
    std::env::remove_var("CARGO_BIN_EXE_enclave");
    assert_eq!(resolve_session_helper_source(), exe);
    std::env::remove_var("ENCLAVE_SELF_EXE");
}

#[test]
fn infer_workspace_helper_from_current_exe_detects_target_debug_binary() {
    let temp = std::env::temp_dir().join(format!(
        "enclave-helper-infer-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let deps = temp.join("target/debug/deps");
    std::fs::create_dir_all(&deps).expect("create deps dir");
    let current_exe = deps.join("integration_suite-abcdef");
    let expected = temp.join("target/debug/enclave");
    std::fs::write(&expected, b"#!/bin/sh\n").expect("write enclave binary placeholder");

    assert_eq!(
        infer_workspace_helper_from_current_exe(&current_exe),
        Some(expected.clone())
    );

    let _ = std::fs::remove_dir_all(temp);
}

#[test]
fn stop_sessions_batch_stops_multiple_term_resistant_processes_with_one_shared_timeout() {
    let mut children = vec![
        spawn_term_resistant_enclave_process(),
        spawn_term_resistant_enclave_process(),
    ];
    let targets = children
        .iter()
        .map(|child| {
            let pid = child.id();
            let starttime = super::process_starttime_ticks(pid).expect("process starttime");
            (pid, Some(starttime))
        })
        .collect::<Vec<_>>();

    let started = Instant::now();
    let result = stop_sessions_batch(&targets).expect("batch stop should succeed");
    let elapsed = started.elapsed();

    for child in &mut children {
        let _ = child.kill();
        let _ = child.wait();
    }

    assert_eq!(result.failed_pids.len(), 0, "no pid should remain running");
    assert_eq!(result.stopped_pids.len(), 2, "both pids should be stopped");
    assert!(
        elapsed < Duration::from_secs(6),
        "batch stop took too long: {:?}; expected one shared timeout window, not serial waits",
        elapsed
    );
}

fn spawn_term_resistant_enclave_process() -> Child {
    Command::new("bash")
        .args([
            "-lc",
            "exec -a enclave-workspace-session bash -lc 'trap \"\" TERM; while :; do sleep 1; done'",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn enclave-like process")
}
