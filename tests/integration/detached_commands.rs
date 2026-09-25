//! The processes a workspace started beyond its own runtime.
//!
//! A workspace owns more than the session runtime. A command it ran leaves
//! descendants behind, and the persistent helper that serves `workspace exec` is a
//! process in the host pid namespace attached to the workspace cgroup. Both are
//! part of the workspace, so both have to be gone before a stop or a destroy can
//! report that it released the workspace.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use enclave::sandbox::{
    create_sandbox, destroy_sandbox, start_sandbox, stop_sandbox, BootstrapMethod,
};
use enclave::workspace::{
    create_workspace, destroy_workspace, exec_workspace_command, start_workspace, stop_workspace,
    WorkspaceLimits,
};

use super::support::{prepare_cached_rootfs, root_only, state_dir, workspace_cgroup_path};

/// The live persistent helpers serving `workspace_id`.
///
/// The helper names its workspace on its command line, and its socket lives in a
/// shared host directory keyed by the runtime, so the workspace id is what makes
/// this specific to one workspace rather than to every helper on the host.
fn persistent_helpers_for(workspace_id: &str) -> Vec<u32> {
    let Ok(entries) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut pids = Vec::new();
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .filter(|name| name.bytes().all(|byte| byte.is_ascii_digit()))
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(cmdline) = fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        let cmdline = String::from_utf8_lossy(&cmdline);
        if cmdline.contains("workspace-session-persistent-helper") && cmdline.contains(workspace_id)
        {
            pids.push(pid);
        }
    }
    pids
}

/// The helper socket for a running workspace, which is keyed by its runtime.
fn helper_socket(runtime_pid: u32, starttime_ticks: u64) -> PathBuf {
    Path::new("/run/enclave").join(format!("session-{runtime_pid}-{starttime_ticks}.sock"))
}

/// A workspace's runtime and everything it started must be gone after a stop, and
/// the helper that serves `workspace exec` is part of that set.
///
/// The helper runs in the host pid namespace, so the runtime exiting does not reap
/// it the way it reaps a process in the workspace's own pid namespace. If a stop
/// missed it, the helper would keep the workspace cgroup alive and hold the
/// socket that the next start of the same workspace would then collide with.
#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn a_stop_reaps_the_persistent_helper_and_its_socket() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-persistent-helper");
    prepare_cached_rootfs(&state, "bookworm");
    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-helper-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    start_sandbox(&state, &sandbox.id).expect("start sandbox");
    let limits = WorkspaceLimits {
        memory_bytes: Some(256 * 1024 * 1024),
        ..WorkspaceLimits::default()
    };
    let workspace = create_workspace(&state, &sandbox.id, "dev", limits).expect("create workspace");
    let started = start_workspace(&state, &sandbox.id, &workspace.id).expect("start workspace");
    let runtime_pid = started.runtime_pid.expect("runtime pid");
    let starttime_ticks = started.runtime_starttime_ticks.expect("runtime start time");

    // The first exec starts the helper, which then stays up to serve later ones.
    let ran = exec_workspace_command(
        &state,
        &sandbox.id,
        &workspace.id,
        "/home",
        &["sh".into(), "-c".into(), "echo served".into()],
    )
    .expect("run a command in the workspace");
    assert_eq!(ran.exit_code, 0, "stderr={}", ran.stderr);

    let helpers = persistent_helpers_for(&workspace.id);
    assert_eq!(
        helpers.len(),
        1,
        "an exec should leave exactly one persistent helper serving {}; found {helpers:?}",
        workspace.id
    );
    let socket = helper_socket(runtime_pid, starttime_ticks);
    assert!(
        socket.exists(),
        "the helper socket {} was never created",
        socket.display()
    );

    stop_workspace(&state, &sandbox.id, &workspace.id).expect("stop workspace");

    assert!(
        persistent_helpers_for(&workspace.id).is_empty(),
        "the stop left the persistent helper running, which holds the workspace cgroup"
    );
    assert!(
        !socket.exists(),
        "the stop left the helper socket {} behind",
        socket.display()
    );
    assert!(
        !workspace_cgroup_path(&sandbox.id, &workspace.id).exists(),
        "the stop left the workspace cgroup behind"
    );

    destroy_workspace(&state, &sandbox.id, &workspace.id).expect("destroy workspace");
    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    destroy_sandbox(&state, &sandbox.id).expect("destroy sandbox");
    let _ = fs::remove_dir_all(state);
}

/// A destroy has to reap the workspace's descendants the same way a stop does.
///
/// A destroy runs a different cleanup path from a stop, and the difference is the
/// kind that hides a leak: a stop that missed a process leaves the cgroup behind
/// and fails loudly, while a destroy that missed one removes the directory the
/// process was described by and leaves it running with nothing to find it by.
#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn a_destroy_reaps_a_descendant_the_workspace_left_running() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-destroy-descendants");
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
        "itest-destroy-descendant-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    start_sandbox(&state, &sandbox.id).expect("start sandbox");
    let limits = WorkspaceLimits {
        memory_bytes: Some(256 * 1024 * 1024),
        ..WorkspaceLimits::default()
    };
    let workspace = create_workspace(&state, &sandbox.id, "dev", limits).expect("create workspace");
    let started = start_workspace(&state, &sandbox.id, &workspace.id).expect("start workspace");
    let runtime_pid = started.runtime_pid.expect("runtime pid");

    let launched = exec_workspace_command(
        &state,
        &sandbox.id,
        &workspace.id,
        "/home",
        &[
            "sh".into(),
            "-c".into(),
            "/bin/busybox sleep 300 >/home/lingering.log 2>&1 & echo launched".into(),
        ],
    )
    .expect("launch background work in the workspace");
    assert_eq!(launched.exit_code, 0, "stderr={}", launched.stderr);

    let cgroup = workspace_cgroup_path(&sandbox.id, &workspace.id);
    let before = super::support::cgroup_processes(&cgroup);
    let descendant = before
        .iter()
        .find(|(pid, _)| *pid != runtime_pid)
        .copied()
        .expect("the backgrounded process must be in the workspace cgroup");

    destroy_workspace(&state, &sandbox.id, &workspace.id).expect("destroy workspace");

    assert_eq!(
        super::support::process_starttime(descendant.0),
        None,
        "the destroy left pid {} from the workspace tree running with nothing describing it",
        descendant.0
    );
    assert!(
        !cgroup.exists(),
        "the destroy left the workspace cgroup {} behind",
        cgroup.display()
    );

    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    destroy_sandbox(&state, &sandbox.id).expect("destroy sandbox");
    let _ = fs::remove_dir_all(state);
}
