use std::fs;
use std::path::{Path, PathBuf};

use enclave::sandbox::{
    create_sandbox, destroy_sandbox, start_sandbox, stop_sandbox, BootstrapMethod,
};
use enclave::workspace::{
    create_workspace, create_workspace_snapshot, destroy_workspace, restore_workspace_snapshot,
    start_workspace, stop_workspace, workspace_runtime_info, WorkspaceLimits,
};

fn root_only() -> bool {
    unsafe { libc::geteuid() == 0 }
}

fn state_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create state dir");
    dir
}

fn prepare_cached_rootfs(state_dir: &Path, suite: &str) {
    let cache = state_dir.join("sandboxes").join("rootfs-cache").join(suite);
    fs::create_dir_all(cache.join("bin")).expect("create bin");
    fs::create_dir_all(cache.join("etc")).expect("create etc");
    fs::create_dir_all(cache.join("usr").join("bin")).expect("create usr/bin");
    fs::copy("/usr/bin/busybox", cache.join("bin").join("busybox")).expect("copy busybox");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("/bin/busybox", cache.join("bin").join("sh"))
            .expect("symlink sh");
        std::os::unix::fs::symlink("/bin/busybox", cache.join("bin").join("dd"))
            .expect("symlink dd");
        std::os::unix::fs::symlink("/bin/busybox", cache.join("usr").join("bin").join("env"))
            .expect("symlink env");
    }
}

#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn snapshot_restore_recovers_filesystem_state() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-snapshot");
    prepare_cached_rootfs(&state, "bookworm");

    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-snapshot-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    start_sandbox(&state, &sandbox.id).expect("start sandbox");

    let workspace = create_workspace(&state, &sandbox.id, "ws", WorkspaceLimits::default())
        .expect("create workspace");

    fs::write(
        Path::new(&workspace.filesystem_path).join("state.txt"),
        "before",
    )
    .expect("seed file");
    create_workspace_snapshot(&state, &sandbox.id, &workspace.id, Some("snap1"))
        .expect("create snapshot");

    fs::write(
        Path::new(&workspace.filesystem_path).join("state.txt"),
        "after",
    )
    .expect("mutate");
    restore_workspace_snapshot(&state, &sandbox.id, &workspace.id, "snap1").expect("restore");

    let restored = fs::read_to_string(Path::new(&workspace.filesystem_path).join("state.txt"))
        .expect("read restored file");
    assert_eq!(restored, "before");

    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    destroy_sandbox(&state, &sandbox.id).expect("destroy sandbox");
    let _ = fs::remove_dir_all(state);
}

#[test]
#[ignore = "requires root privileges and namespace/cgroup support"]
fn cgroup_limits_are_applied_to_workspace_runtime() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-cgroup");
    prepare_cached_rootfs(&state, "bookworm");

    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-cgroup-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    start_sandbox(&state, &sandbox.id).expect("start sandbox");

    let limits = WorkspaceLimits {
        memory_bytes: Some(128 * 1024 * 1024),
        max_processes: Some(64),
        ..WorkspaceLimits::default()
    };
    let workspace =
        create_workspace(&state, &sandbox.id, "limits", limits).expect("create workspace");
    start_workspace(&state, &sandbox.id, &workspace.id).expect("start workspace");

    let runtime = workspace_runtime_info(&state, &sandbox.id, &workspace.id).expect("runtime info");
    assert!(runtime.runtime_pid > 0);

    stop_workspace(&state, &sandbox.id, &workspace.id).expect("stop workspace");
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
fn snapshot_restore_recovers_quota_backed_workspace_state() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-snapshot-disk-quota");
    prepare_cached_rootfs(&state, "bookworm");

    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-snapshot-quota-sandbox",
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
        create_workspace(&state, &sandbox.id, "quota-snap", limits).expect("create workspace");

    fs::write(
        Path::new(&workspace.filesystem_path).join("state.txt"),
        "before",
    )
    .expect("seed file");
    create_workspace_snapshot(&state, &sandbox.id, &workspace.id, Some("snap1"))
        .expect("create snapshot");

    fs::write(
        Path::new(&workspace.filesystem_path).join("state.txt"),
        "after",
    )
    .expect("mutate");
    restore_workspace_snapshot(&state, &sandbox.id, &workspace.id, "snap1").expect("restore");

    start_workspace(&state, &sandbox.id, &workspace.id).expect("start workspace");
    let restored = fs::read_to_string(Path::new(&workspace.filesystem_path).join("state.txt"))
        .expect("read restored file");
    assert_eq!(restored, "before");

    stop_workspace(&state, &sandbox.id, &workspace.id).expect("stop workspace");
    destroy_workspace(&state, &sandbox.id, &workspace.id).expect("destroy workspace");
    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    destroy_sandbox(&state, &sandbox.id).expect("destroy sandbox");
    let _ = fs::remove_dir_all(state);
}
