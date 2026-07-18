use super::WORKSPACE_SESSION_SCRIPT;

#[test]
fn script_execs_pre_pivot_bootstrap_helper_before_ready() {
    assert!(WORKSPACE_SESSION_SCRIPT.contains("internal workspace-session-bootstrap"));
    assert!(WORKSPACE_SESSION_SCRIPT.contains("--rootfs \"$ROOTFS\""));
    assert!(WORKSPACE_SESSION_SCRIPT.contains("--workspace-fs \"$WS_FS\""));
    assert!(WORKSPACE_SESSION_SCRIPT.contains("--mount-target \"$MOUNT_TARGET\""));
    assert!(WORKSPACE_SESSION_SCRIPT.contains("--ready-file \"$READY_FILE\""));
    assert!(WORKSPACE_SESSION_SCRIPT.contains("DISK_BACKED_TMP"));
    assert!(!WORKSPACE_SESSION_SCRIPT.contains("SESSION_HELPER_IN_NS"));
    assert!(!WORKSPACE_SESSION_SCRIPT.contains("pivot_root \"$ROOTFS\""));
}

#[test]
fn script_uses_host_helper_path_instead_of_old_root_exec_path() {
    assert!(WORKSPACE_SESSION_SCRIPT.contains("exec \"$SESSION_HELPER\""));
    assert!(!WORKSPACE_SESSION_SCRIPT.contains("/.old_root${SESSION_HELPER}"));
}

#[test]
fn script_wraps_bootstrap_helper_with_setpriv_when_lsm_options_are_present() {
    assert!(WORKSPACE_SESSION_SCRIPT.contains("exec setpriv $setpriv_args \"$SESSION_HELPER\""));
    assert!(WORKSPACE_SESSION_SCRIPT.contains("--apparmor-profile=$APPARMOR_PROFILE"));
    assert!(WORKSPACE_SESSION_SCRIPT.contains("--selinux-label=$SELINUX_LABEL"));
}

#[test]
fn script_passes_idmapped_mount_option_to_bootstrap_helper() {
    assert!(
        WORKSPACE_SESSION_SCRIPT.contains("--workspace-idmap-option \"$WORKSPACE_IDMAP_OPTION\""),
        "workspace source idmap configuration must be forwarded to the post-pivot bootstrap helper"
    );
    assert!(WORKSPACE_SESSION_SCRIPT.contains("--disk-backed-tmp"));
    assert!(!WORKSPACE_SESSION_SCRIPT.contains("mount --bind \"$WS_FS\" \"$TARGET_DIR\""));
}
