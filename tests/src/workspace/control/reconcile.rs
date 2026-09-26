//! Reconciling a record that disagrees with what is actually running.

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use super::*;

/// A workspace that is starting already owns its address.
///
/// The address is reserved under the registry lock so two workspaces starting at
/// the same time cannot both take the first free one. Counting only running
/// workspaces would let the second one take the address the first just reserved,
/// which is how the batch start path ended up with several workspaces sharing one
/// address.
#[test]
fn used_addresses_include_a_workspace_that_is_only_starting() {
    let mut registry = crate::registry::Registry::default();
    let mut sandbox = crate::registry::RegistrySandbox {
        metadata: sandbox_metadata(std::path::Path::new("/tmp/enclave-ipam-test")),
        workspaces: BTreeMap::new(),
    };
    let mut workspace = workspace_metadata(
        &sandbox.metadata,
        std::path::Path::new("/tmp/enclave-ipam-test/sandboxes/sandbox/workspaces/ws"),
        None,
    );
    workspace.status = WorkspaceStatus::Starting;
    workspace.assigned_ip = Some("10.200.0.10".to_string());
    sandbox.workspaces.insert(workspace.id.clone(), workspace);
    registry
        .sandboxes
        .insert(sandbox.metadata.id.clone(), sandbox);

    let used = collect_all_used_ip_octets(&registry);
    assert!(
        used.contains(&10),
        "a starting workspace's reserved address must not be handed out again"
    );
}

/// A workspace whose runtime died must not keep the interface, rules, or mounts
/// the runtime owned.
///
/// The record is the only description of what to release, so reconcile releases
/// first and clears the record second. When the release cannot be proven — for
/// example without privileges to read the firewall — the record must survive, so
/// the resources stay findable instead of being silently orphaned.
#[test]
fn reconcile_releases_host_resources_for_a_dead_runtime() {
    let temp_dir = std::env::temp_dir().join(format!(
        "enclave-reconcile-dead-runtime-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&temp_dir);
    let sandbox = sandbox_metadata(&temp_dir);
    fs::create_dir_all(&sandbox.sandbox_path).unwrap();
    let workspace_dir = std::path::PathBuf::from(&sandbox.workspaces_path).join("workspace-id");
    fs::create_dir_all(workspace_dir.join("ns")).unwrap();

    let mut workspace = workspace_metadata(&sandbox, &workspace_dir, None);
    workspace.status = WorkspaceStatus::Running;
    workspace.runtime_pid = Some(u32::MAX);
    workspace.runtime_starttime_ticks = Some(1);
    workspace.assigned_ip = Some("10.200.0.42".to_string());

    let repaired = reconcile_workspace_runtime_state(&sandbox, &mut workspace).unwrap();
    if repaired {
        assert_eq!(workspace.status, WorkspaceStatus::Stopped);
        assert!(workspace.runtime_pid.is_none());
        assert!(workspace.runtime_starttime_ticks.is_none());
        assert!(workspace.assigned_ip.is_none());
    } else {
        assert_eq!(workspace.status, WorkspaceStatus::Running);
        assert_eq!(workspace.runtime_pid, Some(u32::MAX));
        assert_eq!(workspace.assigned_ip.as_deref(), Some("10.200.0.42"));
    }
    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn reconcile_clears_dead_runtime_and_namespace_references() {
    let temp_dir = std::env::temp_dir().join(format!(
        "enclave-workspace-runtime-reconcile-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&temp_dir);
    let sandbox_dir = temp_dir.join("sandbox");
    let workspace_dir = sandbox_dir.join("workspaces").join("workspace-id");
    fs::create_dir_all(workspace_dir.join("ns")).unwrap();
    fs::write(workspace_dir.join("ns").join("mnt.ref"), "mnt:[1]\n").unwrap();
    fs::write(workspace_dir.join("ns").join("pid.ref"), "pid:[1]\n").unwrap();

    let mut workspace = WorkspaceMetadata {
        id: "workspace-id".to_string(),
        sandbox_id: "sandbox-id".to_string(),
        name: "workspace".to_string(),
        created_at: "2026-08-06T00:00:00Z".to_string(),
        workspace_path: workspace_dir.to_string_lossy().to_string(),
        filesystem_path: workspace_dir.join("fs").to_string_lossy().to_string(),
        filesystem_mount_target: "/home".to_string(),
        home_mount_source_path: None,
        sandbox_rootfs_path: sandbox_dir.join("rootfs").to_string_lossy().to_string(),
        overlay_home_base_path: sandbox_dir.join("home-base").to_string_lossy().to_string(),
        overlay_home_upper_path: String::new(),
        overlay_home_work_path: String::new(),
        overlay_home_merged_path: String::new(),
        auth_providers: Vec::new(),
        env_tokens: Vec::new(),
        published_ports: Vec::new(),
        status: WorkspaceStatus::Running,
        runtime_pid: Some(u32::MAX),
        runtime_starttime_ticks: Some(1),
        namespace_refs: NamespaceRefs {
            mount: workspace_dir
                .join("ns")
                .join("mnt.ref")
                .to_string_lossy()
                .to_string(),
            pid: workspace_dir
                .join("ns")
                .join("pid.ref")
                .to_string_lossy()
                .to_string(),
        },
        clear_tmp_on_restart: false,
        limits: WorkspaceLimits::default(),
        assigned_ip: Some("10.88.0.99".to_string()),
    };

    assert!(
        reconcile_workspace_runtime_state(&reconcile_sandbox(&sandbox_dir), &mut workspace)
            .unwrap()
    );
    assert_eq!(workspace.status, WorkspaceStatus::Stopped);
    assert!(workspace.runtime_pid.is_none());
    assert!(workspace.runtime_starttime_ticks.is_none());
    assert!(workspace.assigned_ip.is_none());
    assert_eq!(workspace.namespace_refs.mount, "unassigned");
    assert_eq!(workspace.namespace_refs.pid, "unassigned");
    assert!(!workspace_dir.join("ns").join("mnt.ref").exists());
    assert!(!workspace_dir.join("ns").join("pid.ref").exists());

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn workspace_status_helpers_partition_lifecycle_states() {
    for status in [WorkspaceStatus::Starting, WorkspaceStatus::Stopping] {
        assert!(status.is_transitional(), "{status:?} is transitional");
        assert!(!status.is_running(), "{status:?} is not usable");
        assert!(status.may_have_runtime(), "{status:?} may own a runtime");
    }
    assert!(WorkspaceStatus::Running.is_running());
    assert!(!WorkspaceStatus::Running.is_transitional());
    assert!(WorkspaceStatus::Running.may_have_runtime());
    assert!(!WorkspaceStatus::Stopped.may_have_runtime());
    assert!(!WorkspaceStatus::Stopped.is_running());
    assert!(!WorkspaceStatus::Stopped.is_transitional());
    assert_eq!(WorkspaceStatus::Starting.as_str(), "starting");
    assert_eq!(WorkspaceStatus::Stopping.as_str(), "stopping");
    assert_eq!(WorkspaceStatus::Running.as_str(), "running");
    assert_eq!(WorkspaceStatus::Stopped.as_str(), "stopped");
}

#[test]
fn reconcile_rolls_back_interrupted_start_without_a_live_runtime() {
    let temp_dir = std::env::temp_dir().join(format!(
        "enclave-workspace-reconcile-starting-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&temp_dir);
    let sandbox_dir = temp_dir.join("sandbox");
    let (mut workspace, workspace_dir) =
        transitional_workspace(&sandbox_dir, WorkspaceStatus::Starting, None);

    assert!(
        reconcile_workspace_runtime_state(&reconcile_sandbox(&sandbox_dir), &mut workspace)
            .unwrap()
    );
    assert_eq!(workspace.status, WorkspaceStatus::Stopped);
    assert!(workspace.runtime_pid.is_none());
    assert!(workspace.runtime_starttime_ticks.is_none());
    assert!(workspace.assigned_ip.is_none());
    assert_eq!(workspace.namespace_refs.mount, "unassigned");
    assert!(!workspace_dir.join("ns").join("mnt.ref").exists());
    assert!(!workspace_dir.join("ns").join("pid.ref").exists());
    // The rollback is persisted so a second reconcile has nothing left to do.
    assert!(
        !reconcile_workspace_runtime_state(&reconcile_sandbox(&sandbox_dir), &mut workspace)
            .unwrap()
    );

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn reconcile_rolls_back_interrupted_stop_with_a_dead_runtime() {
    let temp_dir = std::env::temp_dir().join(format!(
        "enclave-workspace-reconcile-stopping-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&temp_dir);
    let sandbox_dir = temp_dir.join("sandbox");
    let (mut workspace, workspace_dir) =
        transitional_workspace(&sandbox_dir, WorkspaceStatus::Stopping, Some((u32::MAX, 1)));

    assert!(
        reconcile_workspace_runtime_state(&reconcile_sandbox(&sandbox_dir), &mut workspace)
            .unwrap()
    );
    assert_eq!(workspace.status, WorkspaceStatus::Stopped);
    assert!(workspace.runtime_pid.is_none());
    assert!(workspace.runtime_starttime_ticks.is_none());
    assert!(!workspace_dir.join("ns").join("mnt.ref").exists());

    let _ = fs::remove_dir_all(&temp_dir);
}

/// A launch killed before it commits leaves a live session that only the
/// workspace directory can name.
///
/// The record is written `Starting` with the address reserved and no pid, because
/// the pid is part of the commit the crash interrupted. The session itself wrote
/// `runtime/session.pid` and `ns/pid.ref` from inside its own namespace before it
/// reported ready, so those two files are what the rollback has to read. Without
/// them the teardown runs against a record that names no runtime, and the daemon
/// reports a workspace it has stopped while the session keeps running.
#[test]
fn reconcile_stops_a_runtime_the_record_never_named() {
    let temp_dir = std::env::temp_dir().join(format!(
        "enclave-workspace-reconcile-uncommitted-runtime-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&temp_dir);
    let sandbox_dir = temp_dir.join("sandbox");
    let (mut workspace, workspace_dir) =
        transitional_workspace(&sandbox_dir, WorkspaceStatus::Starting, None);
    assert!(
        workspace.runtime_pid.is_none(),
        "the fixture has to leave the record unnamed for this to test the discovery"
    );

    // A process whose command line marks it as an Enclave runtime, which is what
    // the signal guard checks before it will end one.
    let mut runtime = spawn_enclave_like_runtime();
    let runtime_pid = runtime.id();
    fs::create_dir_all(workspace_dir.join("runtime")).unwrap();
    fs::write(
        workspace_dir.join("runtime").join("session.pid"),
        format!("{runtime_pid}\n"),
    )
    .unwrap();
    // The namespace reference is what proves the pid names the session rather
    // than a process that later inherited the same pid, so it is written from the
    // live process rather than made up.
    fs::write(
        workspace_dir.join("ns").join("pid.ref"),
        format!(
            "{}\n",
            fs::read_link(format!("/proc/{runtime_pid}/ns/pid"))
                .expect("read the runtime's pid namespace")
                .to_string_lossy()
        ),
    )
    .unwrap();

    assert!(
        reconcile_workspace_runtime_state(&reconcile_sandbox(&sandbox_dir), &mut workspace)
            .unwrap()
    );
    assert_eq!(workspace.status, WorkspaceStatus::Stopped);
    assert!(workspace.runtime_pid.is_none());
    assert!(
        reap_within(&mut runtime, Duration::from_secs(10)),
        "the rollback left the runtime the record never named running"
    );
    // The marker files the session wrote are gone with the runtime, so a second
    // reconcile has nothing left to find.
    assert!(!workspace_dir.join("runtime").join("session.pid").exists());

    let _ = fs::remove_dir_all(&temp_dir);
}

/// A process the signal guard recognizes as an Enclave runtime, which exits on
/// the SIGTERM the stop sends it.
fn spawn_enclave_like_runtime() -> Child {
    Command::new("bash")
        .args(["-c", "exec -a enclave-workspace-session sleep 300"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn an enclave-like runtime")
}

/// Whether a child exits within `timeout`.
fn reap_within(child: &mut Child, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if child.try_wait().expect("poll the child").is_some() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
    false
}
