//! The storage a quota-backed workspace gets, and releasing it.
//!
//! A workspace with a disk quota owns an ext4 image on a loop device, and that is
//! the tier whose storage is a real filesystem rather than a directory. These
//! tests check the quota is enforced, that the workspace `/tmp` lives inside the
//! image, that a resize grows it, and that a stop releases the loop device.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

use enclave::sandbox::{
    create_sandbox, destroy_sandbox, start_sandbox, stop_sandbox, BootstrapMethod,
};
use enclave::workspace::{
    create_workspace, destroy_workspace, exec_workspace_command, start_workspace, stop_workspace,
    WorkspaceLimits,
};

use super::support::{prepare_cached_rootfs, root_only, state_dir, SandboxCleanup};

#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn workspace_root_overlay_enforces_total_disk_quota() {
    if !root_only() {
        return;
    }

    use std::io::Write;

    let state = state_dir("enclave-int-root-overlay-quota");
    prepare_cached_rootfs(&state, "bookworm");

    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-root-overlay-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    start_sandbox(&state, &sandbox.id).expect("start sandbox");

    let quota_bytes = 32 * 1024 * 1024;
    let limited = create_workspace(
        &state,
        &sandbox.id,
        "limited",
        WorkspaceLimits {
            disk_bytes: Some(quota_bytes),
            ..WorkspaceLimits::default()
        },
    )
    .expect("create quota workspace");
    let peer = create_workspace(&state, &sandbox.id, "peer", WorkspaceLimits::default())
        .expect("create peer workspace");
    let limited = start_workspace(&state, &sandbox.id, &limited.id).expect("start quota workspace");
    let peer = start_workspace(&state, &sandbox.id, &peer.id).expect("start peer workspace");
    let limited_pid = limited.runtime_pid.expect("quota runtime pid");
    let peer_pid = peer.runtime_pid.expect("peer runtime pid");

    let limited_fill = Path::new("/proc")
        .join(limited_pid.to_string())
        .join("root/opt/quota-fill.bin");
    let peer_fill = Path::new("/proc")
        .join(peer_pid.to_string())
        .join("root/opt/quota-fill.bin");
    let lower_fill = Path::new(&sandbox.rootfs_path).join("opt/quota-fill.bin");
    let block = vec![0u8; 1024 * 1024];
    let mut file = fs::File::create(&limited_fill).expect("create quota fill file");
    let mut wrote_any = false;
    let mut hit_quota = false;
    for _ in 0..64 {
        match file.write_all(&block) {
            Ok(()) => wrote_any = true,
            Err(error) => {
                assert_eq!(error.raw_os_error(), Some(libc::ENOSPC));
                hit_quota = true;
                break;
            }
        }
    }
    assert!(wrote_any, "quota overlay should accept some writes");
    assert!(
        hit_quota,
        "quota overlay should reject writes after exhaustion"
    );
    assert!(
        !peer_fill.exists(),
        "peer workspace must not see quota workspace writes"
    );
    assert!(
        !lower_fill.exists(),
        "shared sandbox lower rootfs must remain unchanged"
    );

    stop_workspace(&state, &sandbox.id, &limited.id).expect("stop quota workspace");
    stop_workspace(&state, &sandbox.id, &peer.id).expect("stop peer workspace");
    destroy_workspace(&state, &sandbox.id, &limited.id).expect("destroy quota workspace");
    destroy_workspace(&state, &sandbox.id, &peer.id).expect("destroy peer workspace");
    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    destroy_sandbox(&state, &sandbox.id).expect("destroy sandbox");
    let _ = fs::remove_dir_all(state);
}

#[test]
#[ignore = "requires root privileges, namespace/mount support, and loopback ext4 mounts"]
fn workspace_disk_quota_caps_enclave_managed_home_storage() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-disk-quota");
    prepare_cached_rootfs(&state, "bookworm");

    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-disk-quota-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    start_sandbox(&state, &sandbox.id).expect("start sandbox");

    let limits = WorkspaceLimits {
        disk_bytes: Some(64 * 1024 * 1024),
        ..WorkspaceLimits::default()
    };
    let workspace =
        create_workspace(&state, &sandbox.id, "quota", limits).expect("create workspace");
    let started = start_workspace(&state, &sandbox.id, &workspace.id).expect("start workspace");
    let runtime_pid = started.runtime_pid.expect("runtime pid");
    let quota_home = Path::new("/proc")
        .join(runtime_pid.to_string())
        .join("root/home");
    let large_file = quota_home.join("fill.bin");
    let payload = vec![b'a'; 1024 * 1024];
    let mut file = std::fs::File::create(&large_file).expect("create fill file");
    let mut wrote_any = false;
    let mut hit_quota = false;
    for _ in 0..80 {
        use std::io::Write;
        match file.write_all(&payload) {
            Ok(()) => wrote_any = true,
            Err(err) => {
                hit_quota = true;
                assert_eq!(
                    err.raw_os_error(),
                    Some(libc::ENOSPC),
                    "expected ENOSPC once workspace quota is exhausted, got: {err}"
                );
                break;
            }
        }
    }
    assert!(
        wrote_any,
        "quota-backed workspace should accept some writes before exhaustion"
    );
    assert!(
        hit_quota,
        "quota-backed workspace should eventually reject writes with ENOSPC"
    );
    assert!(
        large_file.exists(),
        "partial file should exist after quota exhaustion attempt"
    );
    let size = fs::metadata(&large_file).expect("fill file metadata").len();
    assert!(
        size < 80 * 1024 * 1024,
        "quota-backed workspace should stop short of requested size; got {} bytes",
        size
    );

    stop_workspace(&state, &sandbox.id, &workspace.id).expect("stop workspace");
    destroy_workspace(&state, &sandbox.id, &workspace.id).expect("destroy workspace");
    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    destroy_sandbox(&state, &sandbox.id).expect("destroy sandbox");
    let _ = fs::remove_dir_all(state);
}

#[test]
#[ignore = "requires root privileges, namespace/mount support, and loopback ext4 mounts"]
fn workspace_disk_backed_tmp_is_linked_writable_and_uses_workspace_storage() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-disk-backed-tmp");
    let mut cleanup = SandboxCleanup::new(state.clone());
    prepare_cached_rootfs(&state, "bookworm");

    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-disk-backed-tmp-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    cleanup.record(&sandbox.id);
    start_sandbox(&state, &sandbox.id).expect("start sandbox");

    let workspace = create_workspace(
        &state,
        &sandbox.id,
        "tmp",
        WorkspaceLimits {
            disk_bytes: Some(64 * 1024 * 1024),
            ..WorkspaceLimits::default()
        },
    )
    .expect("create workspace");
    let started = start_workspace(&state, &sandbox.id, &workspace.id).expect("start workspace");
    let runtime_pid = started.runtime_pid.expect("runtime pid");
    let runtime_root = Path::new("/proc")
        .join(runtime_pid.to_string())
        .join("root");
    let runtime_tmp = runtime_root.join("tmp");
    let runtime_backing = runtime_root.join("home").join(".enclave-tmp");
    let runtime_home_tmp = runtime_root.join("home/tmp");

    let tmp_metadata = fs::metadata(&runtime_tmp).expect("workspace /tmp metadata");
    let backing_metadata =
        fs::metadata(&runtime_backing).expect("workspace /tmp backing directory metadata");
    assert!(tmp_metadata.is_dir());
    assert!(
        tmp_metadata.nlink() >= 2,
        "workspace /tmp must have a live directory link"
    );
    assert_eq!(tmp_metadata.dev(), backing_metadata.dev());
    assert_eq!(tmp_metadata.ino(), backing_metadata.ino());
    assert_eq!(tmp_metadata.permissions().mode() & 0o1777, 0o1777);
    // The backing directory must not be reachable as `/home/tmp`. When it was,
    // removing that entry unlinked the live `/tmp` mount and every later write
    // under `/tmp` failed with ENOENT.
    assert!(
        !runtime_home_tmp.exists(),
        "the /tmp backing directory must not be aliased in the workspace home view"
    );

    let churn = exec_workspace_command(
        &state,
        &sandbox.id,
        &workspace.id,
        "/home",
        &[
            "sh".to_string(),
            "-c".to_string(),
            "mkdir -p /home/tmp && rm -rf /home/tmp && printf ok > /tmp/enclave-alias-probe && rm /tmp/enclave-alias-probe".to_string(),
        ],
    )
    .expect("churn /home/tmp");
    assert_eq!(
        churn.exit_code, 0,
        "creating and removing /home/tmp must not affect /tmp: {}",
        churn.stderr
    );
    let after_churn = fs::metadata(&runtime_tmp).expect("workspace /tmp metadata after churn");
    assert_eq!(after_churn.ino(), tmp_metadata.ino());
    assert!(after_churn.nlink() >= 2);

    let result = exec_workspace_command(
        &state,
        &sandbox.id,
        &workspace.id,
        "/home",
        &[
            "sh".to_string(),
            "-c".to_string(),
            "test \"$TMPDIR\" = /tmp && printf ok > /tmp/enclave-tmp-probe && test \"$(cat /tmp/enclave-tmp-probe)\" = ok && rm /tmp/enclave-tmp-probe".to_string(),
        ],
    )
    .expect("execute /tmp write probe");
    assert_eq!(
        result.exit_code, 0,
        "workspace command failed: {}",
        result.stderr
    );
}
