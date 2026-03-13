use std::fs;
use std::path::Path;

use enclave::sandbox::{
    create_sandbox, destroy_sandbox, start_sandbox, stop_sandbox, BootstrapMethod,
};
use enclave::workspace::{
    create_workspace_with_options, destroy_workspace, start_workspace, stop_workspace,
    WorkspaceCreateOptions, WorkspaceLimits,
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
fn workspace_host_mount_writes_persist_to_host_directory() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-host-mount");
    prepare_cached_rootfs(&state, "bookworm");
    let host_project = state.join("host-project");
    fs::create_dir_all(&host_project).expect("create host project");
    fs::write(host_project.join("persist.txt"), "before\n").expect("seed host file");

    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-host-mount-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    start_sandbox(&state, &sandbox.id).expect("start sandbox");

    let workspace = create_workspace_with_options(
        &state,
        &sandbox.id,
        "dev",
        WorkspaceCreateOptions {
            limits: WorkspaceLimits::default(),
            home_mount_source: Some(
                host_project
                    .canonicalize()
                    .expect("canonicalize host project")
                    .to_string_lossy()
                    .to_string(),
            ),
            ..WorkspaceCreateOptions::default()
        },
    )
    .expect("create workspace");
    let started = start_workspace(&state, &sandbox.id, &workspace.id).expect("start workspace");
    let runtime_pid = started.runtime_pid.expect("runtime pid should be set");
    let workspace_home = Path::new("/proc")
        .join(runtime_pid.to_string())
        .join("root/home");

    assert_eq!(
        fs::read_to_string(workspace_home.join("persist.txt")).expect("read workspace view"),
        "before\n"
    );

    fs::write(workspace_home.join("persist.txt"), "after\n").expect("write workspace view");
    fs::write(workspace_home.join("created.txt"), "created\n").expect("write new workspace file");

    assert_eq!(
        fs::read_to_string(host_project.join("persist.txt")).expect("read host file"),
        "after\n"
    );
    assert_eq!(
        fs::read_to_string(host_project.join("created.txt")).expect("read new host file"),
        "created\n"
    );

    stop_workspace(&state, &sandbox.id, &workspace.id).expect("stop workspace");
    destroy_workspace(&state, &sandbox.id, &workspace.id).expect("destroy workspace");
    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    destroy_sandbox(&state, &sandbox.id).expect("destroy sandbox");
    let _ = fs::remove_dir_all(state);
}
