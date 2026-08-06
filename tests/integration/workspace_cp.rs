use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};

use enclave::sandbox::{
    create_sandbox, destroy_sandbox, start_sandbox, stop_sandbox, BootstrapMethod,
};
use enclave::workspace::{
    copy_workspace_path, create_workspace, destroy_workspace, start_workspace, stop_workspace,
    WorkspaceLimits,
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
    symlink("/bin/busybox", cache.join("bin").join("sh")).expect("link sh");
    symlink("/bin/busybox", cache.join("usr").join("bin").join("env")).expect("link env");
    symlink("/bin/busybox", cache.join("usr").join("bin").join("tar")).expect("link tar");
    symlink("/bin/busybox", cache.join("usr").join("bin").join("test")).expect("link test");
    symlink("/bin/busybox", cache.join("usr").join("bin").join("mv")).expect("link mv");
}

#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn workspace_cp_streams_files_and_directories_both_directions() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-workspace-cp");
    prepare_cached_rootfs(&state, "bookworm");
    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-cp-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    start_sandbox(&state, &sandbox.id).expect("start sandbox");
    let workspace = create_workspace(&state, &sandbox.id, "cp", WorkspaceLimits::default())
        .expect("create workspace");
    let _started = start_workspace(&state, &sandbox.id, &workspace.id).expect("start workspace");

    let missing_source = copy_workspace_path(
        &state,
        &sandbox.id,
        &workspace.id,
        "/definitely/missing/source",
        "/home/missing",
        "host_to_workspace",
    )
    .expect_err("missing host source should fail");
    assert!(missing_source.to_string().contains("does not exist"));
    let unsafe_destination = copy_workspace_path(
        &state,
        &sandbox.id,
        &workspace.id,
        "/tmp/source",
        "/home/../tmp",
        "host_to_workspace",
    )
    .expect_err("workspace traversal should fail");
    assert!(unsafe_destination
        .to_string()
        .contains("cannot contain '..'"));

    stop_workspace(&state, &sandbox.id, &workspace.id).expect("stop workspace for state check");
    let stopped = copy_workspace_path(
        &state,
        &sandbox.id,
        &workspace.id,
        "/tmp/source",
        "/home/stopped",
        "host_to_workspace",
    )
    .expect_err("copy into stopped workspace should fail");
    assert!(stopped.to_string().contains("is stopped"));
    let started = start_workspace(&state, &sandbox.id, &workspace.id).expect("restart workspace");

    let tar_failure = copy_workspace_path(
        &state,
        &sandbox.id,
        &workspace.id,
        "/home/definitely/missing/source",
        state.join("missing-destination").to_str().unwrap(),
        "workspace_to_host",
    )
    .expect_err("missing workspace source should fail");
    assert!(tar_failure.to_string().contains("tar"));

    let workspace_link = Path::new("/proc")
        .join(started.runtime_pid.unwrap().to_string())
        .join("root/home/link");
    std::os::unix::fs::symlink("/tmp", &workspace_link).expect("create workspace symlink");
    let workspace_symlink = copy_workspace_path(
        &state,
        &sandbox.id,
        &workspace.id,
        "/tmp/source",
        "/home/link/output",
        "host_to_workspace",
    )
    .expect_err("workspace symlink destination should fail");
    assert!(workspace_symlink
        .to_string()
        .contains("symlinked workspace destination"));

    let host_source = state.join("source.txt");
    fs::write(&host_source, "from host").expect("write host source");
    fs::set_permissions(&host_source, fs::Permissions::from_mode(0o640))
        .expect("set source permissions");
    let source_modified = fs::metadata(&host_source)
        .expect("source metadata")
        .modified()
        .expect("source modification time");
    copy_workspace_path(
        &state,
        &sandbox.id,
        &workspace.id,
        host_source.to_str().unwrap(),
        "/home/source.txt",
        "host_to_workspace",
    )
    .expect("copy host file into workspace");
    let workspace_source = Path::new("/proc")
        .join(started.runtime_pid.unwrap().to_string())
        .join("root/home/source.txt");
    assert_eq!(fs::read_to_string(&workspace_source).unwrap(), "from host");
    let workspace_metadata = fs::metadata(&workspace_source).expect("workspace metadata");
    assert_eq!(workspace_metadata.permissions().mode() & 0o777, 0o640);
    assert_eq!(workspace_metadata.modified().unwrap(), source_modified);

    let host_destination = state.join("roundtrip.txt");
    copy_workspace_path(
        &state,
        &sandbox.id,
        &workspace.id,
        "/home/source.txt",
        host_destination.to_str().unwrap(),
        "workspace_to_host",
    )
    .expect("copy workspace file to host");
    assert_eq!(fs::read_to_string(&host_destination).unwrap(), "from host");

    let host_link_target = state.join("host-link-target");
    fs::create_dir(&host_link_target).expect("create host link target");
    let host_link = state.join("host-link");
    std::os::unix::fs::symlink(&host_link_target, &host_link).expect("create host symlink");
    let host_symlink = copy_workspace_path(
        &state,
        &sandbox.id,
        &workspace.id,
        "/home/source.txt",
        host_link.join("output.txt").to_str().unwrap(),
        "workspace_to_host",
    )
    .expect_err("host symlink destination should fail");
    assert!(host_symlink
        .to_string()
        .contains("symlinked host destination"));

    let host_directory = state.join("project");
    fs::create_dir(&host_directory).expect("create host directory");
    fs::write(host_directory.join("nested.txt"), "nested").expect("write nested source");
    copy_workspace_path(
        &state,
        &sandbox.id,
        &workspace.id,
        &format!("{}/", host_directory.display()),
        "/home/project/",
        "host_to_workspace",
    )
    .expect("copy host directory into workspace");
    let workspace_nested = Path::new("/proc")
        .join(started.runtime_pid.unwrap().to_string())
        .join("root/home/project/nested.txt");
    assert_eq!(fs::read_to_string(workspace_nested).unwrap(), "nested");

    stop_workspace(&state, &sandbox.id, &workspace.id).expect("stop workspace");
    destroy_workspace(&state, &sandbox.id, &workspace.id).expect("destroy workspace");
    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    destroy_sandbox(&state, &sandbox.id).expect("destroy sandbox");
    let _ = fs::remove_dir_all(state);
}
