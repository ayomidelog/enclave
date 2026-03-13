use std::fs;
use std::path::{Path, PathBuf};

use enclave::sandbox::{
    create_sandbox, destroy_sandbox, start_sandbox, stop_sandbox, BootstrapMethod,
};
use enclave::workspace::{
    create_workspace, create_workspace_snapshot, restore_workspace_snapshot, start_workspace,
    stop_workspace, workspace_runtime_info, WorkspaceLimits,
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
    fs::create_dir_all(cache.join("usr")).expect("create usr");
    fs::write(cache.join("bin/sh"), "#!/bin/sh\nexit 0\n").expect("write shell");
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
