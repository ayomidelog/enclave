//! Keeping the two memory enforcement layers in step.
//!
//! A workspace memory limit is enforced by the cgroup's `memory.max` and by
//! `RLIMIT_AS` on the session process. The cgroup can be rewritten at any time; the
//! rlimit is set once when the session starts. These tests are about the gap between
//! those two, which is what made a raise on a running workspace not take effect.

use super::*;

use crate::workspace::types::{WorkspaceLimits, WorkspaceMetadata};

const MIB: u64 = 1024 * 1024;

fn workspace_with_memory(memory_bytes: Option<u64>) -> WorkspaceMetadata {
    WorkspaceMetadata {
        id: "ws-1".to_string(),
        sandbox_id: "sb-1".to_string(),
        name: "ws".to_string(),
        created_at: "2026-03-11T00:00:00Z".to_string(),
        workspace_path: "/tmp/enclave-rlimit-test/ws-1".to_string(),
        filesystem_path: "/tmp/enclave-rlimit-test/ws-1/fs".to_string(),
        filesystem_mount_target: "/home".to_string(),
        home_mount_source_path: None,
        sandbox_rootfs_path: "/tmp/enclave-rlimit-test/rootfs".to_string(),
        overlay_home_base_path: "/tmp/enclave-rlimit-test/home-base".to_string(),
        overlay_home_upper_path: String::new(),
        overlay_home_work_path: String::new(),
        overlay_home_merged_path: String::new(),
        auth_providers: Vec::new(),
        owner: None,
        env_tokens: Vec::new(),
        published_ports: Vec::new(),
        status: Default::default(),
        runtime_pid: None,
        runtime_starttime_ticks: None,
        namespace_refs: Default::default(),
        clear_tmp_on_restart: false,
        limits: WorkspaceLimits {
            memory_bytes,
            ..WorkspaceLimits::default()
        },
        assigned_ip: None,
    }
}

/// A child process to point the limit at, which is the same uid so it can be set.
fn spawn_target() -> std::process::Child {
    std::process::Command::new("sleep")
        .arg("30")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn a target process")
}

/// The soft address-space limit the kernel reports for `pid`.
///
/// The kernel prints `unlimited` rather than a number for infinity, so that is read
/// back as the infinity it means instead of as a parse failure.
fn address_space_limit(pid: u32) -> Option<u64> {
    let raw = std::fs::read_to_string(format!("/proc/{pid}/limits")).ok()?;
    let line = raw
        .lines()
        .find(|line| line.starts_with("Max address space"))?;
    // "Max address space   <soft>   <hard>   bytes"
    match line.split_whitespace().nth(3)? {
        "unlimited" => Some(u64::MAX),
        value => value.parse::<u64>().ok(),
    }
}

/// The hard limit has to be roomy for any of this to be testable.
fn hard_limit_is_roomy(pid: u32) -> bool {
    let raw = match std::fs::read_to_string(format!("/proc/{pid}/limits")) {
        Ok(raw) => raw,
        Err(_) => return false,
    };
    let Some(line) = raw
        .lines()
        .find(|line| line.starts_with("Max address space"))
    else {
        return false;
    };
    matches!(line.split_whitespace().nth(4), Some("unlimited"))
}

/// A raise has to reach a process that is already running.
///
/// This is the bug the sync exists for: the cgroup was raised while the process kept
/// the rlimit it started with, so a workspace given more memory still could not use it
/// until it was restarted.
#[test]
fn a_raised_memory_limit_reaches_the_running_process() {
    let mut child = spawn_target();
    let pid = child.id();
    if !hard_limit_is_roomy(pid) {
        let _ = child.kill();
        let _ = child.wait();
        return;
    }

    let workspace = workspace_with_memory(Some(64 * MIB));
    sync_runtime_address_space(&workspace, pid).expect("lower the limit");
    assert_eq!(address_space_limit(pid), Some(64 * MIB));

    // The raise, which is the case that used to be lost.
    let raised = workspace_with_memory(Some(512 * MIB));
    sync_runtime_address_space(&raised, pid).expect("raise the limit");
    assert_eq!(
        address_space_limit(pid),
        Some(512 * MIB),
        "a raised limit must reach the running process"
    );

    let _ = child.kill();
    let _ = child.wait();
}

/// A limit that is already correct costs no write, which is what keeps this free on
/// the start path where the session has just set it.
#[test]
fn an_already_correct_limit_is_left_alone() {
    let mut child = spawn_target();
    let pid = child.id();
    if !hard_limit_is_roomy(pid) {
        let _ = child.kill();
        let _ = child.wait();
        return;
    }

    let workspace = workspace_with_memory(Some(128 * MIB));
    sync_runtime_address_space(&workspace, pid).expect("set the limit");
    let before = address_space_limit(pid);
    sync_runtime_address_space(&workspace, pid).expect("set it again");
    assert_eq!(address_space_limit(pid), before);
    assert_eq!(before, Some(128 * MIB));

    let _ = child.kill();
    let _ = child.wait();
}

/// A workspace with no memory limit has no rlimit either, and clearing one has to
/// reach the running process just as raising it does.
#[test]
fn removing_the_memory_limit_reaches_the_running_process() {
    let mut child = spawn_target();
    let pid = child.id();
    if !hard_limit_is_roomy(pid) {
        let _ = child.kill();
        let _ = child.wait();
        return;
    }

    let limited = workspace_with_memory(Some(64 * MIB));
    sync_runtime_address_space(&limited, pid).expect("set the limit");
    assert_eq!(address_space_limit(pid), Some(64 * MIB));

    let unlimited = workspace_with_memory(None);
    sync_runtime_address_space(&unlimited, pid).expect("clear the limit");
    assert_eq!(
        address_space_limit(pid),
        Some(u64::MAX),
        "a cleared limit must reach the running process"
    );

    let _ = child.kill();
    let _ = child.wait();
}
