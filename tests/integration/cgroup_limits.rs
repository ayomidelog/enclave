//! The kernel limits a workspace declares, as the runtime sees them.
//!
//! A limit that is recorded but not applied is worse than no limit, because the
//! record says the workspace is bounded. These tests read the values back from the
//! runtime's own cgroup rather than from the metadata.

use std::fs;
use std::path::Path;

use enclave::sandbox::{
    create_sandbox, destroy_sandbox, start_sandbox, stop_sandbox, BootstrapMethod,
};
use enclave::workspace::{
    create_workspace, exec_workspace_command, start_workspace, stop_workspace,
    workspace_runtime_info, WorkspaceLimits,
};

use super::support::{persistent_helper_is_running, prepare_cached_rootfs, root_only, state_dir};

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
        cpu_percent: Some(10.0),
        memory_bytes: Some(128 * 1024 * 1024),
        max_processes: Some(64),
        ..WorkspaceLimits::default()
    };
    let workspace =
        create_workspace(&state, &sandbox.id, "limits", limits).expect("create workspace");
    start_workspace(&state, &sandbox.id, &workspace.id).expect("start workspace");

    let runtime = workspace_runtime_info(&state, &sandbox.id, &workspace.id).expect("runtime info");
    assert!(runtime.runtime_pid > 0);

    let cgroup_path = runtime
        .cgroup_path
        .as_deref()
        .expect("a limited workspace must report its cgroup");
    assert!(
        Path::new(cgroup_path).is_dir(),
        "{cgroup_path} is not a cgroup directory"
    );
    let cpu_max = fs::read_to_string(Path::new(cgroup_path).join("cpu.max")).expect("read cpu.max");
    assert_ne!(
        cpu_max.split_whitespace().next().unwrap_or_default(),
        "max",
        "cpu_percent must cap the workspace cgroup: {cpu_max}"
    );

    // The daemon-managed exec path runs commands through a persistent helper;
    // the commands it forks must inherit the workspace cgroup, otherwise the
    // declared limits would not apply to them.
    let result = exec_workspace_command(
        &state,
        &sandbox.id,
        &workspace.id,
        "/home",
        &[
            "sh".to_string(),
            "-c".to_string(),
            "cat /proc/self/cgroup".to_string(),
        ],
    )
    .expect("run workspace command");
    assert_eq!(result.exit_code, 0, "stderr: {}", result.stderr);
    let expected = format!("enclave-ws-{}-{}", sandbox.id, workspace.id);
    assert!(
        result.stdout.contains(&expected),
        "expected the command to run inside {expected}, got: {}",
        result.stdout
    );

    // A helper that outlives its runtime would hold the workspace cgroup and
    // its mount namespace forever, so it has to notice the runtime's death and
    // exit on its own.
    let helper_socket = format!(
        "/run/enclave/session-{}-{}.sock",
        runtime.runtime_pid, runtime.runtime_starttime_ticks
    );
    assert!(
        persistent_helper_is_running(&helper_socket),
        "expected a live helper for {helper_socket}"
    );
    unsafe { libc::kill(runtime.runtime_pid as i32, libc::SIGKILL) };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let mut exited = false;
    while std::time::Instant::now() < deadline {
        if !persistent_helper_is_running(&helper_socket) {
            exited = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    assert!(
        exited,
        "the persistent helper must exit once its runtime is gone"
    );

    stop_workspace(&state, &sandbox.id, &workspace.id).expect("stop workspace");
    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    destroy_sandbox(&state, &sandbox.id).expect("destroy sandbox");
    let _ = fs::remove_dir_all(state);
}
