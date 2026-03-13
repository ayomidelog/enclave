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
