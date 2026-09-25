use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Instant;

use enclave::sandbox::{
    create_sandbox, destroy_sandbox, start_sandbox, stop_sandbox, BootstrapMethod,
};
use enclave::workspace::{
    create_workspace, destroy_workspace, exec_workspace_command, start_workspace, stop_workspace,
    WorkspaceLimits, WorkspaceStatus,
};

fn root_only() -> bool {
    unsafe { libc::geteuid() == 0 }
}

fn prepare_cached_rootfs(state_dir: &Path, suite: &str) {
    let cache = state_dir.join("sandboxes").join("rootfs-cache").join(suite);
    fs::create_dir_all(cache.join("bin")).expect("create bin");
    fs::create_dir_all(cache.join("etc")).expect("create etc");
    fs::create_dir_all(cache.join("usr")).expect("create usr");
    fs::create_dir_all(cache.join("usr/bin")).expect("create usr bin");
    fs::copy("/usr/bin/busybox", cache.join("bin/busybox")).expect("copy busybox");
    std::os::unix::fs::symlink("busybox", cache.join("bin/sh")).expect("link shell");
    std::os::unix::fs::symlink("../../bin/busybox", cache.join("usr/bin/env")).expect("link env");
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

/// A sandbox whose rootfs bind mount is missing must not hand a workspace an
/// empty root.
///
/// The bind mount the session receives is host state, and the registry cannot
/// show whether it is present: a stop/start cycle or an interrupted teardown can
/// leave the sandbox recorded as running with no rootfs mounted. Before the
/// pre-flight existed the workspace started against an empty directory and was
/// still reported as running, which is a silent failure rather than a reported
/// one.
#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn workspace_start_repairs_a_missing_sandbox_rootfs_mount() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-rootfs-bind");
    prepare_cached_rootfs(&state, "bookworm");

    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-rootfs-bind-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    let running = start_sandbox(&state, &sandbox.id).expect("start sandbox");
    let workspace = create_workspace(&state, &sandbox.id, "dev", WorkspaceLimits::default())
        .expect("create workspace");

    // Detach the bind mount the daemon owns, leaving the sandbox recorded as
    // running with no root filesystem attached.
    let mounted_rootfs = std::ffi::CString::new(running.mounted_rootfs_path.as_str())
        .expect("rootfs path has no interior nul");
    let rc = unsafe { libc::umount2(mounted_rootfs.as_ptr(), 0) };
    assert_eq!(
        rc,
        0,
        "detaching the sandbox rootfs bind failed: {}",
        std::io::Error::last_os_error()
    );
    assert!(
        !is_mountpoint(&running.mounted_rootfs_path),
        "the bind mount must be gone before the workspace start"
    );

    let started = start_workspace(&state, &sandbox.id, &workspace.id).expect("start workspace");
    assert_eq!(started.status, WorkspaceStatus::Running);
    assert!(
        is_mountpoint(&running.mounted_rootfs_path),
        "the pre-flight must restore the sandbox rootfs bind mount"
    );

    // The workspace must see the sandbox rootfs rather than an empty directory.
    let result = exec_workspace_command(
        &state,
        &sandbox.id,
        &workspace.id,
        "/",
        &[
            "sh".to_string(),
            "-c".to_string(),
            "test -x /bin/sh && test -d /usr && echo rootfs-ok".to_string(),
        ],
    )
    .expect("execute the workspace rootfs probe");
    assert_eq!(
        result.exit_code, 0,
        "stdout={} stderr={}",
        result.stdout, result.stderr
    );
    assert!(
        result.stdout.contains("rootfs-ok"),
        "stdout={}",
        result.stdout
    );

    // The sandbox cgroup cannot be removed while a workspace cgroup sits under
    // it, so the workspace goes first.
    destroy_workspace(&state, &sandbox.id, &workspace.id).expect("destroy workspace");
    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    destroy_sandbox(&state, &sandbox.id).expect("destroy sandbox");
    let _ = fs::remove_dir_all(state);
}

/// Whether the mount table currently lists `path` as a mount point.
fn is_mountpoint(path: &str) -> bool {
    let Ok(raw) = fs::read_to_string("/proc/self/mountinfo") else {
        return false;
    };
    raw.lines()
        .any(|line| line.split_whitespace().nth(4) == Some(path))
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
    let start_started = Instant::now();
    let started = start_workspace(&state, &sandbox.id, &workspace.id).expect("start workspace");
    println!(
        "benchmark=workspace_start_warm elapsed_seconds={:.6}",
        start_started.elapsed().as_secs_f64()
    );
    let exec_started = Instant::now();
    for _ in 0..3 {
        let result = exec_workspace_command(
            &state,
            &sandbox.id,
            &workspace.id,
            "/home",
            &["sh".into(), "-c".into(), "exit 0".into()],
        )
        .expect("persistent workspace exec should succeed");
        assert_eq!(
            result.exit_code, 0,
            "stdout={} stderr={}",
            result.stdout, result.stderr
        );
    }
    println!(
        "benchmark=workspace_exec_persistent iterations=3 elapsed_seconds={:.6} average_seconds={:.6}",
        exec_started.elapsed().as_secs_f64(),
        exec_started.elapsed().as_secs_f64() / 3.0
    );
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

/// Every interface named by an Enclave-owned firewall rule, with its rule count.
fn enclave_rules_by_interface() -> Option<Vec<(String, usize)>> {
    let saved = Command::new("iptables-save").output().ok()?;
    if !saved.status.success() {
        return None;
    }
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for line in String::from_utf8_lossy(&saved.stdout).lines() {
        if !line.contains("enclave:") {
            continue;
        }
        for field in line.split_whitespace() {
            if field.starts_with("veth-") {
                *counts.entry(field.to_string()).or_default() += 1;
            }
        }
    }
    Some(counts.into_iter().collect())
}

#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn starting_a_workspace_releases_its_dead_runtime_rules() {
    if !root_only() {
        return;
    }
    let Some(before) = enclave_rules_by_interface() else {
        return;
    };

    let state = state_dir("enclave-int-dead-runtime");
    prepare_cached_rootfs(&state, "bookworm");
    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-dead-runtime-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    start_sandbox(&state, &sandbox.id).expect("start sandbox");
    let workspace = create_workspace(&state, &sandbox.id, "dev", WorkspaceLimits::default())
        .expect("create workspace");
    let started = start_workspace(&state, &sandbox.id, &workspace.id).expect("start workspace");
    let runtime_pid = started.runtime_pid.expect("runtime pid") as i32;

    // Kill the runtime the way a crash would, leaving the registry saying the
    // workspace is still running. The kernel releases the interface with the
    // namespace, but the rules that name it are Enclave's to remove.
    unsafe { libc::kill(runtime_pid, libc::SIGKILL) };
    for _ in 0..50 {
        if unsafe { libc::kill(runtime_pid, 0) } != 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    start_workspace(&state, &sandbox.id, &workspace.id)
        .expect("a start must release the dead runtime and succeed");

    let after = enclave_rules_by_interface().expect("read rules after the restart");
    // Every rule the probe's own run added is keyed by the workspace id hash, so
    // only interfaces new since the first snapshot belong to this test.
    let added = after
        .iter()
        .filter(|(interface, _)| !before.iter().any(|(known, _)| known == interface))
        .map(|(interface, _)| interface.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        added.len(),
        1,
        "the restarted workspace should own exactly one interface; found {added:?}"
    );
    assert!(
        Path::new("/sys/class/net").join(&added[0]).exists(),
        "the rule names interface {} which does not exist",
        added[0]
    );

    stop_workspace(&state, &sandbox.id, &workspace.id).expect("stop workspace");
    destroy_workspace(&state, &sandbox.id, &workspace.id).expect("destroy workspace");
    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    destroy_sandbox(&state, &sandbox.id).expect("destroy sandbox");
    let _ = fs::remove_dir_all(state);
}
