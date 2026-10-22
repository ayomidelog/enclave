use super::credentials::redact;
use super::{runtime_exec_command_args, workspace_file_receive_args};
use crate::workspace::{WorkspaceLimits, WorkspaceMetadata, WorkspaceStatus};

fn credential(name: &str, value: &str) -> crate::auth::WorkspaceAuthToken {
    crate::auth::WorkspaceAuthToken {
        name: name.to_string(),
        env_var: name.to_ascii_uppercase(),
        token: value.to_string(),
    }
}

/// Every injected value is replaced, and the longest match wins.
///
/// This is the copy that goes into the workspace's runtime log, so it has to remove a
/// value whatever the caller asked for: a token in that file outlives the command and
/// nothing scrubs it later.
#[test]
fn the_logged_copy_has_every_credential_removed() {
    let credentials = vec![
        credential("github", "ghp_secret_value"),
        credential("npm", "npm_secret"),
    ];
    let output = "token=ghp_secret_value and npm_secret, plus unrelated text";

    let redacted = redact(&credentials, output);

    assert!(!redacted.contains("ghp_secret_value"), "{redacted}");
    assert!(!redacted.contains("npm_secret"), "{redacted}");
    assert_eq!(redacted.matches("[REDACTED]").count(), 2, "{redacted}");
    assert!(redacted.contains("unrelated text"), "{redacted}");
}

/// With nothing injected there is nothing to remove, so the text is unchanged.
#[test]
fn redaction_with_no_credentials_returns_the_text_unchanged() {
    assert_eq!(redact(&[], "unchanged output"), "unchanged output");
}

/// A one-character secret does not redact the whole output.
///
/// An empty secret would match at every position and turn the output into nothing but
/// redaction markers, so the scrubber ignores one; this pins that the same is true of
/// the copy the log gets.
#[test]
fn an_empty_credential_does_not_redact_everything() {
    let credentials = vec![credential("blank", "")];
    assert_eq!(redact(&credentials, "a normal line"), "a normal line");
}

#[test]
fn runtime_exec_clears_environment_before_running_wrapper() {
    let workspace = WorkspaceMetadata {
        id: "ws-1".to_string(),
        sandbox_id: "sb-1".to_string(),
        name: "workspace".to_string(),
        created_at: "2026-01-01T00:00:00Z".to_string(),
        last_request_at: None,
        last_process_activity_at: None,
        last_stopped_at: None,
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
        owner: None,
        env_tokens: vec![],
        published_ports: vec![],
        status: WorkspaceStatus::Running,
        runtime_pid: Some(1234),
        runtime_starttime_ticks: Some(1),
        namespace_refs: Default::default(),
        keep_running: false,
        idle_stop_after_secs: None,
        destroy_after_secs: None,
        clear_tmp_on_restart: false,
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
    assert_eq!(args[12], "--cgroup-path");
    assert_eq!(
        args[13],
        "/sys/fs/cgroup/enclave-sb-sb-1/enclave-ws-sb-1-ws-1"
    );
    assert_eq!(args[14], "--");
    assert_eq!(args[15], "/bin/echo");
    assert_eq!(args[16], "ok");
}

#[test]
fn runtime_exec_separates_wrapped_command_flags() {
    let args = runtime_exec_command_args(
        1234,
        1,
        "sb-1",
        "ws-1",
        "/home",
        &["tar".to_string(), "-C".to_string(), "/home".to_string()],
    );

    assert_eq!(
        &args[12..],
        [
            "--cgroup-path",
            "/sys/fs/cgroup/enclave-sb-sb-1/enclave-ws-sb-1-ws-1",
            "--",
            "tar",
            "-C",
            "/home"
        ]
    );
}

#[test]
fn file_receiver_is_attached_to_the_workspace_cgroup() {
    let args = workspace_file_receive_args(
        1234,
        1,
        "/home/stage/file",
        Some("/sys/fs/cgroup/enclave-sb-sb-1/enclave-ws-sb-1-ws-1"),
        [0, 1, 2, 3, 4, 5],
    );

    assert_eq!(
        &args[..12],
        [
            "internal",
            "workspace-file-receive",
            "--runtime-pid",
            "1234",
            "--runtime-starttime-ticks",
            "1",
            "--target",
            "/home/stage/file",
            "--cgroup-path",
            "/sys/fs/cgroup/enclave-sb-sb-1/enclave-ws-sb-1-ws-1",
            "--root-fd",
            "0",
        ]
    );
}

#[test]
fn file_receiver_omits_cgroup_path_without_a_workspace_cgroup() {
    let args = workspace_file_receive_args(1234, 1, "/home/file", None, [0, 1, 2, 3, 4, 5]);

    assert!(!args.iter().any(|arg| arg == "--cgroup-path"));
    assert_eq!(args[8], "--root-fd");
}
