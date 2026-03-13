use super::*;
use anyhow::anyhow;
use std::path::Path;

#[test]
fn denied_syscalls_include_escape_primitives() {
    let syscalls = denied_syscalls(false);
    assert!(syscalls.contains(&(libc::SYS_ptrace as u32)));
    assert!(syscalls.contains(&(libc::SYS_setns as u32)));
    assert!(syscalls.contains(&(libc::SYS_unshare as u32)));
    assert!(syscalls.contains(&(libc::SYS_mount as u32)));
    assert!(syscalls.contains(&(libc::SYS_bpf as u32)));
    assert!(syscalls.contains(&(libc::SYS_clone3 as u32)));
}

#[test]
fn exec_capabilities_cover_expected_subset() {
    let keep: BTreeSet<u32> = EXEC_CAPABILITIES.iter().copied().collect();
    assert!(keep.contains(&CAP_CHOWN));
    assert!(keep.contains(&CAP_SETUID));
    assert!(keep.contains(&CAP_SETGID));
    assert!(
        !keep.contains(&21),
        "CAP_SYS_ADMIN must not remain available"
    );
}

#[test]
fn masked_targets_cover_kernel_info_leaks() {
    assert!(MASK_FILE_TARGETS.contains(&"/proc/kallsyms"));
    assert!(MASK_FILE_TARGETS.contains(&"/proc/modules"));
    assert!(MASK_DIR_TARGETS.contains(&"/sys/module"));
}

#[test]
fn exec_seccomp_filter_allows_clone3_for_user_command_compatibility() {
    let syscalls = denied_syscalls(true);
    assert!(!syscalls.contains(&(libc::SYS_clone3 as u32)));
}

#[test]
fn readonly_remount_policy_ignores_eperm_for_sys_mounts() {
    let err = anyhow!(std::io::Error::from_raw_os_error(libc::EPERM));
    assert!(should_ignore_readonly_remount_error(
        Path::new("/sys"),
        &err
    ));
    assert!(should_ignore_readonly_remount_error(
        Path::new("/sys/fs/cgroup"),
        &err
    ));
}

#[test]
fn readonly_remount_policy_keeps_proc_sys_strict() {
    let err = anyhow!(std::io::Error::from_raw_os_error(libc::EPERM));
    assert!(!should_ignore_readonly_remount_error(
        Path::new("/proc/sys"),
        &err
    ));
}

#[test]
fn readonly_remount_policy_keeps_non_eperm_errors_strict() {
    let err = anyhow!(std::io::Error::from_raw_os_error(libc::EINVAL));
    assert!(!should_ignore_readonly_remount_error(
        Path::new("/sys"),
        &err
    ));
}
