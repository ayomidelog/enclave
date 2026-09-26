use super::*;
use crate::workspace::DEFAULT_WORKSPACE_PATH;

#[test]
fn sanitize_cwd_allows_workspace_paths() {
    assert_eq!(sanitize_workspace_cwd("/home"), "/home");
    assert_eq!(sanitize_workspace_cwd("/home/src"), "/home/src");
}

#[test]
fn sanitize_cwd_rejects_outside_paths() {
    assert_eq!(sanitize_workspace_cwd("/etc"), "/home");
    assert_eq!(sanitize_workspace_cwd("/tmp"), "/home");
    assert_eq!(sanitize_workspace_cwd("/root"), "/home");
    assert_eq!(sanitize_workspace_cwd("/"), "/home");
    assert_eq!(sanitize_workspace_cwd("/projects2"), "/home");
    assert_eq!(sanitize_workspace_cwd("/home-old"), "/home");
}

#[test]
fn sanitize_cwd_rejects_parent_traversal() {
    assert_eq!(sanitize_workspace_cwd("/home/../../etc"), "/home");
}

#[test]
fn sanitize_cwd_rejects_controls_and_normalizes() {
    assert_eq!(sanitize_workspace_cwd("/home/\n/evil"), "/home");
    assert_eq!(sanitize_workspace_cwd("/home//src"), "/home/src");
    assert_eq!(sanitize_workspace_cwd("/home/./src"), "/home/src");
    assert_eq!(sanitize_workspace_cwd("/home/"), "/home");
}

#[test]
fn is_executable_rejects_relative_paths() {
    assert!(!is_executable_in_rootfs("/nonexistent", "relative/path"));
}

#[test]
fn shell_path_validation_rejects_invalid_values() {
    assert!(!is_valid_shell_path(""));
    assert!(!is_valid_shell_path("bash -x"));
    assert!(!is_valid_shell_path("relative"));
    assert!(!is_valid_shell_path("/bin/\nsh"));
    assert!(is_valid_shell_path("/bin/sh"));
}

#[test]
fn default_workspace_path_includes_flutter_bin() {
    assert!(DEFAULT_WORKSPACE_PATH.contains("/opt/flutter/bin"));
}

fn runtime_info(cgroup_path: Option<&str>) -> WorkspaceRuntimeInfo {
    WorkspaceRuntimeInfo {
        sandbox_id: "sb-1".to_string(),
        workspace_id: "ws-1".to_string(),
        workspace_name: "workspace".to_string(),
        runtime_pid: 4321,
        runtime_starttime_ticks: 99,
        sandbox_rootfs_path: "/tmp/rootfs".to_string(),
        cgroup_path: cgroup_path.map(str::to_string),
    }
}

#[test]
fn exec_helper_is_attached_to_the_workspace_cgroup() {
    let args = internal_workspace_command_args(
        &runtime_info(Some("/sys/fs/cgroup/enclave-sb-sb-1/enclave-ws-sb-1-ws-1")),
        "/home",
        &["/bin/echo".to_string(), "ok".to_string()],
    );

    assert_eq!(
        args,
        [
            "internal",
            "workspace-command",
            "--runtime-pid",
            "4321",
            "--runtime-starttime-ticks",
            "99",
            "--cwd",
            "/home",
            "--sandbox-id",
            "sb-1",
            "--workspace-id",
            "ws-1",
            "--cgroup-path",
            "/sys/fs/cgroup/enclave-sb-sb-1/enclave-ws-sb-1-ws-1",
            "/bin/echo",
            "ok",
        ]
    );
}

#[test]
fn exec_helper_omits_cgroup_path_when_the_host_has_no_workspace_cgroup() {
    let args =
        internal_workspace_command_args(&runtime_info(None), "/home", &["/bin/true".to_string()]);

    assert!(!args.iter().any(|arg| arg == "--cgroup-path"));
    assert_eq!(args.last().map(String::as_str), Some("/bin/true"));
}
