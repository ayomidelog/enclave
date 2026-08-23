use std::fs;
use std::path::Path;

use enclave::sandbox::{
    create_sandbox, destroy_sandbox, start_sandbox, stop_sandbox, BootstrapMethod,
};
use enclave::workspace::{
    create_workspace, create_workspace_with_options, destroy_workspace, start_workspace,
    stop_workspace, WorkspaceCreateOptions, WorkspaceLimits,
};

fn root_only() -> bool {
    unsafe { libc::geteuid() == 0 }
}

fn prepare_cached_rootfs(state_dir: &Path, suite: &str) {
    let cache = state_dir.join("sandboxes").join("rootfs-cache").join(suite);
    fs::create_dir_all(cache.join("bin")).expect("create bin");
    fs::create_dir_all(cache.join("etc")).expect("create etc");
    fs::create_dir_all(cache.join("usr")).expect("create usr");
    fs::write(cache.join("bin/sh"), "#!/bin/sh\nexit 0\n").expect("write shell");
}

fn state_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create state dir");
    dir
}

#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn workspaces_do_not_share_tmp_directory_contents() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-tmp-isolation");
    prepare_cached_rootfs(&state, "bookworm");

    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-tmp-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    start_sandbox(&state, &sandbox.id).expect("start sandbox");

    let workspace_a = create_workspace(&state, &sandbox.id, "alpha", WorkspaceLimits::default())
        .expect("create workspace alpha");
    let workspace_b = create_workspace(&state, &sandbox.id, "beta", WorkspaceLimits::default())
        .expect("create workspace beta");

    let started_a = start_workspace(&state, &sandbox.id, &workspace_a.id).expect("start alpha");
    let started_b = start_workspace(&state, &sandbox.id, &workspace_b.id).expect("start beta");

    let root_a = Path::new("/proc")
        .join(
            started_a
                .runtime_pid
                .expect("alpha runtime pid")
                .to_string(),
        )
        .join("root");
    let root_b = Path::new("/proc")
        .join(started_b.runtime_pid.expect("beta runtime pid").to_string())
        .join("root");

    fs::write(root_a.join("tmp").join("alpha-only.txt"), "alpha\n").expect("write alpha tmp file");

    assert!(
        !root_b.join("tmp").join("alpha-only.txt").exists(),
        "workspace beta should not see workspace alpha's /tmp file"
    );

    stop_workspace(&state, &sandbox.id, &workspace_a.id).expect("stop alpha");
    stop_workspace(&state, &sandbox.id, &workspace_b.id).expect("stop beta");
    destroy_workspace(&state, &sandbox.id, &workspace_a.id).expect("destroy alpha");
    destroy_workspace(&state, &sandbox.id, &workspace_b.id).expect("destroy beta");
    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    destroy_sandbox(&state, &sandbox.id).expect("destroy sandbox");
    let _ = fs::remove_dir_all(state);
}

#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn quota_workspace_tmp_is_cleared_after_restart() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-tmp-reset");
    prepare_cached_rootfs(&state, "bookworm");
    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-tmp-reset-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    start_sandbox(&state, &sandbox.id).expect("start sandbox");
    let workspace = create_workspace_with_options(
        &state,
        &sandbox.id,
        "reset",
        WorkspaceCreateOptions {
            limits: WorkspaceLimits {
                disk_bytes: Some(32 * 1024 * 1024),
                ..WorkspaceLimits::default()
            },
            clear_tmp_on_restart: true,
            ..WorkspaceCreateOptions::default()
        },
    )
    .expect("create workspace");
    let started = start_workspace(&state, &sandbox.id, &workspace.id).expect("start workspace");
    let runtime_root = Path::new("/proc")
        .join(started.runtime_pid.expect("runtime pid").to_string())
        .join("root");
    let temporary_file = runtime_root.join("tmp/restart-me");
    fs::write(&temporary_file, "temporary\n").expect("write workspace tmp file");
    assert!(temporary_file.exists());

    stop_workspace(&state, &sandbox.id, &workspace.id).expect("stop workspace");
    start_workspace(&state, &sandbox.id, &workspace.id).expect("restart workspace");
    let restarted = enclave::workspace::workspace_runtime_info(&state, &sandbox.id, &workspace.id)
        .expect("read restarted workspace runtime");
    let restarted_file = Path::new("/proc")
        .join(restarted.runtime_pid.to_string())
        .join("root/tmp/restart-me");
    assert!(
        !restarted_file.exists(),
        "workspace /tmp must reset on restart"
    );

    stop_workspace(&state, &sandbox.id, &workspace.id).expect("stop workspace");
    destroy_workspace(&state, &sandbox.id, &workspace.id).expect("destroy workspace");
    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    destroy_sandbox(&state, &sandbox.id).expect("destroy sandbox");
    let _ = fs::remove_dir_all(state);
}
