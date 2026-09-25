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
/// Absolute path of the cgroup that holds a workspace's processes.
///
/// The naming is a contract the daemon and doctor both rely on, so the test
/// builds it from the ids rather than reaching into the crate.
fn workspace_cgroup_path(sandbox_id: &str, workspace_id: &str) -> std::path::PathBuf {
    Path::new("/sys/fs/cgroup")
        .join(format!("enclave-sb-{sandbox_id}"))
        .join(format!("enclave-ws-{sandbox_id}-{workspace_id}"))
}

/// Field 22 of /proc/<pid>/stat: the process start time in clock ticks.
///
/// The command name can contain spaces and parentheses, so the fields are counted
/// from the last closing parenthesis rather than from the start of the line.
fn process_starttime(pid: u32) -> Option<u64> {
    let raw = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    raw.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()
}

/// The host PIDs in a cgroup, each with the start time that identifies it across
/// PID reuse.
fn cgroup_processes(cgroup: &Path) -> Vec<(u32, u64)> {
    let Ok(raw) = fs::read_to_string(cgroup.join("cgroup.procs")) else {
        return Vec::new();
    };
    raw.lines()
        .filter_map(|line| line.trim().parse::<u32>().ok())
        .filter_map(|pid| process_starttime(pid).map(|starttime| (pid, starttime)))
        .collect()
}

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

/// A workspace record whose runtime identity is stale must never signal whatever
/// process holds that pid now.
///
/// A pid is not an identity: the kernel reuses pids, so a record that kept only
/// the number would let a stop kill an unrelated process that happened to inherit
/// it. The record carries the process start time as well, and this test points the
/// record at a live process whose start time does not match, then proves the stop
/// leaves that process untouched.
#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn stopping_a_workspace_never_signals_a_pid_it_does_not_own() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-pid-reuse");
    prepare_cached_rootfs(&state, "bookworm");
    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-pid-reuse-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    start_sandbox(&state, &sandbox.id).expect("start sandbox");
    let workspace = create_workspace(&state, &sandbox.id, "dev", WorkspaceLimits::default())
        .expect("create workspace");
    // The workspace is deliberately never started. The record written below is the
    // shape an interrupted start leaves behind: it claims a running runtime, and
    // the pid it names has since been handed to an unrelated process.

    // An unrelated process, standing in for whatever holds the pid after reuse.
    let mut victim = Command::new("sleep")
        .arg("300")
        .spawn()
        .expect("spawn the unrelated process");
    let victim_pid = victim.id();
    let victim_starttime = process_starttime(victim_pid).expect("victim start time");

    // Point the record at the victim with a start time that is not its own. A pid
    // on its own cannot tell the process the record meant apart from whatever holds
    // the number now, which is why the start time is part of the identity.
    enclave::registry::with_registry_mut(&state, |registry| {
        let sandbox = registry
            .sandboxes
            .get_mut(&sandbox.id)
            .expect("sandbox record");
        let workspace = sandbox
            .workspaces
            .get_mut(&workspace.id)
            .expect("workspace record");
        workspace.status = WorkspaceStatus::Running;
        workspace.runtime_pid = Some(victim_pid);
        workspace.runtime_starttime_ticks = Some(victim_starttime.wrapping_add(1));
        Ok(())
    })
    .expect("record the stale runtime identity");

    stop_workspace(&state, &sandbox.id, &workspace.id).expect("stop workspace");

    assert_eq!(
        process_starttime(victim_pid),
        Some(victim_starttime),
        "the stop signalled pid {victim_pid}, which it does not own"
    );

    let _ = victim.kill();
    let _ = victim.wait();

    destroy_workspace(&state, &sandbox.id, &workspace.id).expect("destroy workspace");
    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    destroy_sandbox(&state, &sandbox.id).expect("destroy sandbox");
    let _ = fs::remove_dir_all(state);
}

/// Repair must not delete a workspace whose metadata file is gone while its
/// runtime is still alive.
///
/// The registry and the per-directory metadata are two copies of one record, and
/// repair rebuilds the registry from the tree. A workspace directory with no
/// `workspace.json` therefore looks like an orphan to the scan, but the runtime it
/// left behind is real, and deleting the directory would leave that runtime, its
/// cgroup, its interface, and its firewall rules owned by nothing. The directory is
/// retained and reported instead, and discovery reads only the markers the runtime
/// itself wrote, so it never signals a process to find out.
#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn repair_retains_a_workspace_whose_metadata_is_gone_but_whose_runtime_lives() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-missing-workspace-metadata");
    prepare_cached_rootfs(&state, "bookworm");
    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-missing-metadata-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    start_sandbox(&state, &sandbox.id).expect("start sandbox");
    let workspace = create_workspace(&state, &sandbox.id, "dev", WorkspaceLimits::default())
        .expect("create workspace");
    let started = start_workspace(&state, &sandbox.id, &workspace.id).expect("start workspace");
    let runtime_pid = started.runtime_pid.expect("runtime pid");

    // Remove only the per-directory copy. The registry still records the workspace
    // and the runtime is untouched, which is the disagreement repair has to resolve.
    let metadata_path = Path::new(&workspace.workspace_path).join("workspace.json");
    fs::remove_file(&metadata_path).expect("remove the workspace metadata");

    let report = enclave::registry::repair_registry(&state, false).expect("repair the registry");

    assert!(
        Path::new(&workspace.workspace_path).is_dir(),
        "repair deleted the directory of a workspace whose runtime is still alive"
    );
    let retained = report
        .retained_orphans
        .iter()
        .find(|orphan| orphan.workspace_id == workspace.id)
        .unwrap_or_else(|| {
            panic!(
                "repair must report the retained workspace; report was {}",
                serde_json::to_string_pretty(&report).unwrap_or_default()
            )
        });
    assert_eq!(
        retained.runtime.runtime_pid,
        Some(runtime_pid),
        "repair must identify the live runtime from the markers it wrote"
    );
    assert_eq!(
        process_starttime(runtime_pid),
        started.runtime_starttime_ticks,
        "repair signalled the workspace runtime it was supposed to leave alone"
    );

    // The scan skipped this directory, and repair makes the registry describe what
    // the scan found, so the workspace is no longer managed by the registry and this
    // test releases what it created itself.
    unsafe { libc::kill(runtime_pid as i32, libc::SIGKILL) };
    for _ in 0..50 {
        if process_starttime(runtime_pid).is_none() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let _ = fs::remove_dir(workspace_cgroup_path(&sandbox.id, &workspace.id));
    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    let _ = destroy_sandbox(&state, &sandbox.id);
    let _ = fs::remove_dir_all(state);
}

/// Repair must rebuild a workspace record that only the registry lost.
///
/// The per-directory metadata is what the scan reads, so a record deleted from the
/// registry is recoverable from the directory it describes. The rebuilt record has
/// to carry the runtime identity the file holds: a record that came back without it
/// would describe a workspace that is running as one that is not, and the next stop
/// would look for nothing to release.
#[test]
#[ignore = "requires root privileges and namespace/mount support"]
fn repair_rebuilds_a_workspace_record_the_registry_lost() {
    if !root_only() {
        return;
    }

    let state = state_dir("enclave-int-missing-registry-record");
    prepare_cached_rootfs(&state, "bookworm");
    let sandbox = create_sandbox(
        &state,
        "debootstrap",
        "itest-missing-record-sandbox",
        "bookworm",
        "http://deb.debian.org/debian",
        &BootstrapMethod::CachedRootfs,
    )
    .expect("create sandbox");
    start_sandbox(&state, &sandbox.id).expect("start sandbox");
    let workspace = create_workspace(&state, &sandbox.id, "dev", WorkspaceLimits::default())
        .expect("create workspace");
    let started = start_workspace(&state, &sandbox.id, &workspace.id).expect("start workspace");

    enclave::registry::with_registry_mut(&state, |registry| {
        registry
            .sandboxes
            .get_mut(&sandbox.id)
            .expect("sandbox record")
            .workspaces
            .remove(&workspace.id);
        Ok(())
    })
    .expect("drop the workspace record");

    let report = enclave::registry::repair_registry(&state, false).expect("repair the registry");
    assert_eq!(
        report.added_workspaces, 1,
        "repair must report rebuilding exactly the workspace record the registry lost"
    );

    let rebuilt = enclave::registry::with_registry(&state, |registry| {
        Ok(registry
            .sandboxes
            .get(&sandbox.id)
            .and_then(|sandbox| sandbox.workspaces.get(&workspace.id))
            .cloned())
    })
    .expect("read the repaired registry")
    .expect("repair must restore the workspace record from its directory");
    assert_eq!(
        rebuilt.runtime_pid, started.runtime_pid,
        "the rebuilt record must carry the runtime the workspace is actually running"
    );
    assert_eq!(
        rebuilt.runtime_starttime_ticks,
        started.runtime_starttime_ticks
    );

    stop_workspace(&state, &sandbox.id, &workspace.id).expect("stop workspace");
    destroy_workspace(&state, &sandbox.id, &workspace.id).expect("destroy workspace");
    stop_sandbox(&state, &sandbox.id).expect("stop sandbox");
    destroy_sandbox(&state, &sandbox.id).expect("destroy sandbox");
    let _ = fs::remove_dir_all(state);
}
