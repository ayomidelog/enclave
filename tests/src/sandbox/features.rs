use super::*;

#[test]
fn overlayfs_check_does_not_panic() {
    let _ = check_overlayfs();
}

#[test]
fn mount_namespace_check_does_not_panic() {
    let _ = check_mount_namespace();
}

#[test]
fn pid_namespace_check_does_not_panic() {
    let _ = check_pid_namespace();
}

#[test]
fn net_namespace_check_does_not_panic() {
    let _ = check_net_namespace();
}

#[test]
fn user_namespace_check_does_not_panic() {
    let _ = check_user_namespace();
}

#[test]
fn cgroup_v2_check_returns_bool() {
    let _result: bool = check_cgroup_v2();
}

#[test]
fn validate_platform_does_not_panic() {
    let _ = validate_platform();
}
