use std::fs;
use std::path::Path;
use std::process::Command;

use enclave::sandbox::{
    create_sandbox, destroy_sandbox, start_sandbox, stop_sandbox, BootstrapMethod,
};
use enclave::workspace::{
    create_workspace, destroy_workspace, start_workspace, stop_workspace, WorkspaceLimits,
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
fn sandbox_lifecycle_create_start_stop_destroy() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-sandbox");
    prepare_cached_rootfs(&state, "bookworm");

    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    assert!(Path::new(&sandbox.rootfs_path).exists());

    let started = start_sandbox(&state, &sandbox.id).expect("start sandbox");
    assert!(Path::new(&started.mounted_rootfs_path).exists());

    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    destroy_sandbox(&state, &sandbox.id).expect("destroy sandbox");
    let _ = fs::remove_dir_all(state);
}

#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn workspace_lifecycle_create_start_stop_destroy() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-workspace");
    prepare_cached_rootfs(&state, "bookworm");

    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-workspace-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    start_sandbox(&state, &sandbox.id).expect("start sandbox");

    let workspace = create_workspace(&state, &sandbox.id, "dev", WorkspaceLimits::default())
        .expect("create workspace");
    let started = start_workspace(&state, &sandbox.id, &workspace.id).expect("start workspace");
    let runtime_pid = started.runtime_pid.expect("runtime pid should be set");
    let route_table = Command::new("nsenter")
        .arg("--net")
        .arg("--target")
        .arg(runtime_pid.to_string())
        .arg("--")
        .arg("ip")
        .arg("route")
        .arg("show")
        .arg("default")
        .output()
        .expect("inspect workspace default route");
    assert!(
        route_table.status.success(),
        "nsenter ip route show default failed: {}",
        String::from_utf8_lossy(&route_table.stderr)
    );
    let route_stdout = String::from_utf8_lossy(&route_table.stdout);
    assert!(
        route_stdout
            .lines()
            .any(|line| line.contains("default") && line.contains("dev eth0")),
        "workspace must have a default route on eth0; got:\n{}",
        route_stdout
    );

    stop_workspace(&state, &sandbox.id, &workspace.id).expect("stop workspace");
    destroy_workspace(&state, &sandbox.id, &workspace.id).expect("destroy workspace");
    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    destroy_sandbox(&state, &sandbox.id).expect("destroy sandbox");
    let _ = fs::remove_dir_all(state);
}
