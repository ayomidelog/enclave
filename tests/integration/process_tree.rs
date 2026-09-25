//! Taking a workspace's process tree with it when it stops.

use std::fs;
use std::process::Command;

use enclave::sandbox::{
    create_sandbox, destroy_sandbox, start_sandbox, stop_sandbox, BootstrapMethod,
};
use enclave::workspace::{
    create_workspace, destroy_workspace, exec_workspace_command, start_workspace, stop_workspace,
    WorkspaceLimits,
};

use super::support::{
    cgroup_processes, prepare_cached_rootfs, process_starttime, root_only, state_dir,
    workspace_cgroup_path,
};

/// A workspace owns more than its runtime. A command that backgrounds work leaves
/// descendants in the workspace cgroup, and a stop has to take the whole tree: a
/// survivor keeps the workspace's cgroup, mounts, and network namespace alive
/// after the registry already says the workspace is stopped.
#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn stopping_a_workspace_reaps_its_descendant_processes() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-descendants");
    prepare_cached_rootfs(&state, "bookworm");
    // A backgrounded command redirects its standard input from /dev/null, so the
    // minimal rootfs needs one before it can leave work running behind it.
    let dev = state.join("sandboxes/rootfs-cache/bookworm/dev");
    fs::create_dir_all(&dev).expect("create dev");
    let _ = fs::remove_file(dev.join("null"));
    assert!(
        Command::new("mknod")
            .arg(dev.join("null"))
            .args(["c", "1", "3"])
            .status()
            .expect("run mknod")
            .success(),
        "failed to create /dev/null in the test rootfs"
    );

    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-descendant-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    start_sandbox(&state, &sandbox.id).expect("start sandbox");
    // A memory limit is what makes the workspace own a cgroup, and the cgroup is
    // how the test finds the processes the workspace owns.
    let limits = WorkspaceLimits {
        memory_bytes: Some(256 * 1024 * 1024),
        ..WorkspaceLimits::default()
    };
    let workspace = create_workspace(&state, &sandbox.id, "dev", limits).expect("create workspace");
    start_workspace(&state, &sandbox.id, &workspace.id).expect("start workspace");

    // The command that starts the work exits immediately, so the only processes
    // left behind are the ones it backgrounded. The rootfs is a busybox shell, so
    // the sleeping processes are started through busybox itself. Each one sends
    // its output to a file rather than inheriting the command's pipe: a
    // background process that keeps the pipe open would hold the exec until it
    // exits, which is what the workspace's own output collection is waiting on.
    let launched = exec_workspace_command(
        &state,
        &sandbox.id,
        &workspace.id,
        "/home",
        &[
            "sh".into(),
            "-c".into(),
            "/bin/busybox sleep 300 >/home/one.log 2>&1 & \
             /bin/busybox sleep 300 >/home/two.log 2>&1 & \
             echo launched"
                .into(),
        ],
    )
    .expect("launch background work in the workspace");
    assert_eq!(launched.exit_code, 0, "stderr={}", launched.stderr);

    let cgroup = workspace_cgroup_path(&sandbox.id, &workspace.id);
    let before = cgroup_processes(&cgroup);
    assert!(
        before.len() >= 3,
        "expected the runtime and two background descendants in {}, got {before:?}",
        cgroup.display()
    );

    stop_workspace(&state, &sandbox.id, &workspace.id).expect("stop workspace");

    let survivors = before
        .iter()
        .filter(|(pid, starttime)| process_starttime(*pid) == Some(*starttime))
        .collect::<Vec<_>>();
    assert!(
        survivors.is_empty(),
        "stop left process(es) {survivors:?} from the workspace tree alive"
    );
    assert!(
        !cgroup.exists(),
        "stop left the workspace cgroup {} behind",
        cgroup.display()
    );

    destroy_workspace(&state, &sandbox.id, &workspace.id).expect("destroy workspace");
    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    destroy_sandbox(&state, &sandbox.id).expect("destroy sandbox");
    let _ = fs::remove_dir_all(state);
}

/// A stop must remove the whole workspace tree even when a process in it refuses
/// to die on TERM.
///
/// The runtime exits on TERM, so the signal alone is enough for it, but the
/// processes a workspace left behind are not required to be polite. A descendant
/// that ignores TERM survives the signal, and only the fallback that follows it —
/// the cgroup kill, and the verified SIGKILL after that — removes it. Without that
/// fallback a stop could report success while a process kept the workspace's
/// cgroup, mounts, and network namespace alive.
#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn stopping_a_workspace_removes_a_descendant_that_ignores_term() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-term-ignoring-descendant");
    prepare_cached_rootfs(&state, "bookworm");
    // A backgrounded command redirects its standard input from /dev/null, so the
    // minimal rootfs needs one before it can leave work running behind it.
    let dev = state.join("sandboxes/rootfs-cache/bookworm/dev");
    fs::create_dir_all(&dev).expect("create dev");
    let _ = fs::remove_file(dev.join("null"));
    assert!(
        Command::new("mknod")
            .arg(dev.join("null"))
            .args(["c", "1", "3"])
            .status()
            .expect("run mknod")
            .success(),
        "failed to create /dev/null in the test rootfs"
    );

    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-term-ignoring-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    start_sandbox(&state, &sandbox.id).expect("start sandbox");
    // A memory limit is what makes the workspace own a cgroup, and the cgroup is
    // how the test finds the processes the workspace owns.
    let limits = WorkspaceLimits {
        memory_bytes: Some(256 * 1024 * 1024),
        ..WorkspaceLimits::default()
    };
    let workspace = create_workspace(&state, &sandbox.id, "dev", limits).expect("create workspace");
    let started = start_workspace(&state, &sandbox.id, &workspace.id).expect("start workspace");

    // `trap "" TERM` sets the disposition to ignore, and an ignored disposition
    // survives exec, so the sleep that replaces the shell ignores TERM as well.
    let launched = exec_workspace_command(
        &state,
        &sandbox.id,
        &workspace.id,
        "/home",
        &[
            "sh".into(),
            "-c".into(),
            "/bin/busybox sh -c 'trap \"\" TERM; exec /bin/busybox sleep 300' \
             >/home/stuck.log 2>&1 & echo launched"
                .into(),
        ],
    )
    .expect("launch a TERM-ignoring process in the workspace");
    assert_eq!(launched.exit_code, 0, "stderr={}", launched.stderr);

    let cgroup = workspace_cgroup_path(&sandbox.id, &workspace.id);
    let before = cgroup_processes(&cgroup);
    let runtime_pid = started.runtime_pid.expect("runtime pid");
    let descendant = before
        .iter()
        .find(|(pid, _)| *pid != runtime_pid)
        .copied()
        .expect("the TERM-ignoring descendant must be in the workspace cgroup");

    // Prove the premise before asserting on the outcome. If the descendant did
    // not ignore TERM the stop would pass for the wrong reason and this test would
    // stop covering the fallback it exists to cover.
    unsafe { libc::kill(descendant.0 as i32, libc::SIGTERM) };
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert_eq!(
        process_starttime(descendant.0),
        Some(descendant.1),
        "pid {} was expected to ignore TERM; the stop below would not reach the fallback",
        descendant.0
    );

    stop_workspace(&state, &sandbox.id, &workspace.id).expect("stop workspace");

    let survivors = before
        .iter()
        .filter(|(pid, starttime)| process_starttime(*pid) == Some(*starttime))
        .collect::<Vec<_>>();
    assert!(
        survivors.is_empty(),
        "stop left process(es) {survivors:?} from the workspace tree alive, including pid {} which ignores TERM",
        descendant.0
    );
    assert!(
        !cgroup.exists(),
        "stop left the workspace cgroup {} behind",
        cgroup.display()
    );

    destroy_workspace(&state, &sandbox.id, &workspace.id).expect("destroy workspace");
    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    destroy_sandbox(&state, &sandbox.id).expect("destroy sandbox");
    let _ = fs::remove_dir_all(state);
}
