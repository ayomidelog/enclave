//! The individual checks, run against the state each one is about.
//!
//! Several of these only prove the check survives an empty or missing state: a
//! check that panics on a fresh host would take the whole report down with it.

use super::*;

fn running_workspace_fixture(name: &str, runtime_pid: u32) -> std::path::PathBuf {
    use crate::registry::RegistrySandbox;
    use crate::sandbox::{BootstrapMethod, SandboxLimits, SandboxMetadata, SandboxStatus};
    use std::collections::BTreeMap;

    let state = std::env::temp_dir().join(format!(
        "enclave-doctor-{name}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let _ = std::fs::remove_dir_all(&state);
    crate::registry::ensure_registry(&state).expect("create registry");
    let sandbox_dir = state.join("sandboxes").join("sandbox-id");
    let workspace_dir = sandbox_dir.join("workspaces").join("workspace-id");
    std::fs::create_dir_all(&workspace_dir).expect("create workspace fixture");
    let sandbox = SandboxMetadata {
        id: "sandbox-id".to_string(),
        name: "box".to_string(),
        suite: "bookworm".to_string(),
        mirror: "http://deb.debian.org/debian".to_string(),
        bootstrap_method: BootstrapMethod::CachedRootfs,
        created_at: "2026-08-06T00:00:00Z".to_string(),
        sandbox_path: sandbox_dir.to_string_lossy().to_string(),
        rootfs_path: sandbox_dir.join("rootfs").to_string_lossy().to_string(),
        rootfs_lower_path: None,
        mounted_rootfs_path: sandbox_dir.join("rootfs.mnt").to_string_lossy().to_string(),
        workspaces_path: sandbox_dir.join("workspaces").to_string_lossy().to_string(),
        home_base_path: sandbox_dir.join("home-base").to_string_lossy().to_string(),
        limits: SandboxLimits::default(),
        status: SandboxStatus::Running,
    };
    let workspace = crate::workspace::WorkspaceMetadata {
        id: "workspace-id".to_string(),
        sandbox_id: sandbox.id.clone(),
        name: "workspace".to_string(),
        created_at: "2026-08-06T00:00:00Z".to_string(),
        last_request_at: Some(chrono::Utc::now().to_rfc3339()),
        last_process_activity_at: None,
        last_stopped_at: None,
        workspace_path: workspace_dir.to_string_lossy().to_string(),
        filesystem_path: workspace_dir.join("fs").to_string_lossy().to_string(),
        filesystem_mount_target: "/home".to_string(),
        home_mount_source_path: None,
        sandbox_rootfs_path: sandbox.rootfs_path.clone(),
        overlay_home_base_path: sandbox.home_base_path.clone(),
        overlay_home_upper_path: String::new(),
        overlay_home_work_path: String::new(),
        overlay_home_merged_path: String::new(),
        auth_providers: Vec::new(),
        owner: None,
        env_tokens: Vec::new(),
        published_ports: Vec::new(),
        status: crate::workspace::WorkspaceStatus::Running,
        runtime_pid: Some(runtime_pid),
        runtime_starttime_ticks: crate::workspace::session::process_starttime_ticks(runtime_pid)
            .ok(),
        namespace_refs: Default::default(),
        clear_tmp_on_restart: false,
        keep_running: false,
        idle_stop_after_secs: None,
        destroy_after_secs: None,
        limits: Default::default(),
        assigned_ip: None,
    };
    crate::registry::with_registry_mut(&state, |registry| {
        registry.sandboxes.insert(
            sandbox.id.clone(),
            RegistrySandbox {
                metadata: sandbox,
                workspaces: BTreeMap::from([(workspace.id.clone(), workspace)]),
            },
        );
        Ok(())
    })
    .expect("insert workspace fixture");
    state
}

#[test]
fn task_count_threshold_is_reported() {
    let state = running_workspace_fixture("tasks", std::process::id());
    let check = crate::doctor::activity::check_task_counts_with(&state, 10, |_| Some(11));
    assert_eq!(check.status, "warn");
    assert!(check.detail.contains("11 tasks"), "{}", check.detail);
    let _ = std::fs::remove_dir_all(state);
}

#[test]
fn detached_process_threshold_is_reported() {
    let state = running_workspace_fixture("detached", std::process::id());
    let check = crate::doctor::activity::check_detached_processes_with(&state, 0, |_| 1);
    assert_eq!(check.status, "warn");
    assert!(
        check.detail.contains("1 detached process"),
        "{}",
        check.detail
    );
    let _ = std::fs::remove_dir_all(state);
}

#[test]
fn workspace_activity_reports_idle_but_skips_live_ports_and_pins() {
    use crate::registry::RegistrySandbox;
    use crate::sandbox::{BootstrapMethod, SandboxLimits, SandboxMetadata, SandboxStatus};
    use std::collections::BTreeMap;

    let state = std::env::temp_dir().join(format!(
        "enclave-doctor-activity-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let _ = std::fs::remove_dir_all(&state);
    crate::registry::ensure_registry(&state).expect("create registry");
    let sandbox_dir = state.join("sandboxes/sandbox-id");
    let metadata = SandboxMetadata {
        id: "sandbox-id".to_string(),
        name: "box".to_string(),
        suite: "bookworm".to_string(),
        mirror: "http://deb.debian.org/debian".to_string(),
        bootstrap_method: BootstrapMethod::CachedRootfs,
        created_at: "2026-08-06T00:00:00Z".to_string(),
        sandbox_path: sandbox_dir.to_string_lossy().to_string(),
        rootfs_path: sandbox_dir.join("rootfs").to_string_lossy().to_string(),
        rootfs_lower_path: None,
        mounted_rootfs_path: sandbox_dir.join("rootfs.mnt").to_string_lossy().to_string(),
        workspaces_path: sandbox_dir.join("workspaces").to_string_lossy().to_string(),
        home_base_path: sandbox_dir.join("home-base").to_string_lossy().to_string(),
        limits: SandboxLimits::default(),
        status: SandboxStatus::Running,
    };
    let old = (chrono::Utc::now() - chrono::Duration::seconds(600)).to_rfc3339();
    let workspace = |id: &str| crate::workspace::WorkspaceMetadata {
        id: id.to_string(),
        sandbox_id: metadata.id.clone(),
        name: id.to_string(),
        created_at: "2026-08-06T00:00:00Z".to_string(),
        last_request_at: Some(old.clone()),
        last_process_activity_at: None,
        last_stopped_at: None,
        workspace_path: sandbox_dir
            .join("workspaces")
            .join(id)
            .to_string_lossy()
            .to_string(),
        filesystem_path: String::new(),
        filesystem_mount_target: "/home".to_string(),
        home_mount_source_path: None,
        sandbox_rootfs_path: metadata.rootfs_path.clone(),
        overlay_home_base_path: metadata.home_base_path.clone(),
        overlay_home_upper_path: String::new(),
        overlay_home_work_path: String::new(),
        overlay_home_merged_path: String::new(),
        auth_providers: Vec::new(),
        owner: None,
        env_tokens: Vec::new(),
        published_ports: Vec::new(),
        status: crate::workspace::WorkspaceStatus::Running,
        runtime_pid: None,
        runtime_starttime_ticks: None,
        namespace_refs: Default::default(),
        clear_tmp_on_restart: false,
        keep_running: id == "pinned",
        idle_stop_after_secs: None,
        destroy_after_secs: None,
        limits: Default::default(),
        assigned_ip: None,
    };
    let mut workspaces = BTreeMap::new();
    for id in ["idle", "pinned", "serving"] {
        workspaces.insert(id.to_string(), workspace(id));
    }
    crate::registry::with_registry_mut(&state, |registry| {
        registry.sandboxes.insert(
            metadata.id.clone(),
            RegistrySandbox {
                metadata,
                workspaces,
            },
        );
        Ok(())
    })
    .expect("insert workspace fixture");

    let check = crate::doctor::activity::check_workspace_activity(
        &state,
        Some(60),
        |_, id| id == "pinned",
        |_, id| id == "serving",
        |_, _| 0,
    );
    assert_eq!(check.status, "warn", "{}", check.detail);
    assert!(check.detail.contains("idle"), "{}", check.detail);
    assert!(!check.detail.contains("pinned"), "{}", check.detail);
    assert!(!check.detail.contains("serving"), "{}", check.detail);

    let _ = std::fs::remove_dir_all(state);
}

#[test]
fn check_stale_cgroups_does_not_panic() {
    let state_dir = std::env::temp_dir().join(format!(
        "enclave-doctor-cgroups-test-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&state_dir).expect("create doctor fixture");
    crate::registry::ensure_registry(&state_dir).expect("create registry fixture");
    let check = check_stale_cgroups(&state_dir);
    assert!(
        check.status == "ok" || check.status == "warn",
        "unexpected status: {}",
        check.status
    );
    std::fs::remove_dir_all(state_dir).expect("remove doctor fixture");
}

#[test]
fn check_orphaned_mounts_does_not_panic() {
    let tmp = std::env::temp_dir().join(format!("enclave-doctor-test-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let check = check_orphaned_mounts(&tmp);
    assert!(
        check.status == "ok" || check.status == "warn",
        "unexpected status: {}",
        check.status
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn check_sandbox_rootfs_reports_an_active_sandbox_without_a_rootfs_mount() {
    use crate::registry::{with_registry_mut, RegistrySandbox};
    use crate::sandbox::{BootstrapMethod, SandboxLimits, SandboxMetadata, SandboxStatus};
    use std::collections::BTreeMap;

    let state = std::env::temp_dir().join(format!(
        "enclave-doctor-rootfs-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&state);
    let sandbox_dir = state.join("sandboxes").join("sandbox-id");
    std::fs::create_dir_all(sandbox_dir.join("runtime")).unwrap();

    let metadata = SandboxMetadata {
        id: "sandbox-id".to_string(),
        name: "sandbox".to_string(),
        suite: "bookworm".to_string(),
        mirror: "https://deb.debian.org/debian".to_string(),
        bootstrap_method: BootstrapMethod::CachedRootfs,
        created_at: "2026-08-06T00:00:00Z".to_string(),
        sandbox_path: sandbox_dir.to_string_lossy().to_string(),
        rootfs_path: sandbox_dir.join("rootfs").to_string_lossy().to_string(),
        rootfs_lower_path: None,
        mounted_rootfs_path: sandbox_dir
            .join("runtime")
            .join("rootfs.mnt")
            .to_string_lossy()
            .to_string(),
        workspaces_path: sandbox_dir.join("workspaces").to_string_lossy().to_string(),
        home_base_path: sandbox_dir.join("home-base").to_string_lossy().to_string(),
        limits: SandboxLimits::default(),
        status: SandboxStatus::Stopped,
    };

    // A stopped sandbox is not expected to serve a root filesystem.
    crate::registry::ensure_registry(&state).unwrap();
    with_registry_mut(&state, |registry| {
        registry.sandboxes.insert(
            metadata.id.clone(),
            RegistrySandbox {
                metadata: metadata.clone(),
                workspaces: BTreeMap::new(),
            },
        );
        Ok(())
    })
    .unwrap();
    let check = check_sandbox_rootfs(&state);
    assert_eq!(check.status, "ok", "stopped sandbox: {}", check.detail);

    // A running sandbox whose bind mount is missing cannot serve a workspace.
    std::fs::create_dir_all(&metadata.mounted_rootfs_path).unwrap();
    with_registry_mut(&state, |registry| {
        registry
            .sandboxes
            .get_mut(&metadata.id)
            .unwrap()
            .metadata
            .status = SandboxStatus::Running;
        Ok(())
    })
    .unwrap();
    let check = check_sandbox_rootfs(&state);
    assert_eq!(check.status, "warn");
    assert!(check.detail.contains("is not mounted"), "{}", check.detail);

    let _ = std::fs::remove_dir_all(&state);
}

/// Report Enclave-owned rules whose interface no longer exists.
///
/// A rule is scoped to an interface, so once that interface is gone the rule can
/// never match again. The ownership comment is what proves Enclave installed it,
/// so a rule in this list is safe to remove without touching anything the host or
/// another tool installed.
///
/// The shared bridge rules are excluded: they are scoped to the bridge and to the
/// subnet rather than to one workspace interface, and `ensure_nat` already retires
/// the shapes it does not install.
#[test]
fn stale_firewall_rules_are_scoped_to_removed_interfaces() {
    let owned = |table: &str, owner: &str, rule: &str| crate::network::nat::OwnedRule {
        table: table.to_string(),
        chain: "INPUT".to_string(),
        owner: owner.to_string(),
        rule: rule.to_string(),
    };

    // A live interface: /sys/class/net/lo always exists on Linux.
    let live = owned("filter", "session-a", "! -s 10.200.0.4/32 -i lo -j DROP");
    let gone = owned(
        "filter",
        "session-b",
        "! -s 10.200.0.5/32 -i veth-99-abcdef -j DROP",
    );
    // The shared bridge rules name the bridge, not a workspace interface.
    let shared = owned("filter", "bridge", "-i enclave0 -o enclave0 -j DROP");

    let rules = vec![live.clone(), gone.clone(), shared.clone()];
    let stale = stale_anti_spoof_rules(&rules);
    assert_eq!(stale.len(), 1, "only the removed interface is stale");
    assert_eq!(stale[0].owner, "session-b");

    // A rule with no interface, or with more than one, is left alone: this repair
    // only acts when the rule names exactly one interface it can resolve.
    let no_interface = owned("filter", "session-c", "-s 10.200.0.6/32 -j DROP");
    let two_interfaces = owned(
        "filter",
        "session-d",
        "-i veth-99-abcdef -i veth-98-abcdef -j DROP",
    );
    assert!(stale_anti_spoof_rules(&[no_interface, two_interfaces]).is_empty());

    // The check itself must survive a host without iptables or without the
    // privilege to read the rules: it reports rather than panics.
    let check = check_stale_firewall_rules();
    assert!(
        check.status == "ok" || check.status == "warn",
        "unexpected status: {}",
        check.status
    );
}

#[test]
fn operation_journal_check_reports_unfinished_and_malformed_records() {
    let state_dir = std::env::temp_dir().join(format!(
        "enclave-doctor-journal-test-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(state_dir.join("operations")).expect("create journal fixture");
    let record = crate::operation::OperationRecord::new("workspace.stop", "sb/ws");
    std::fs::write(
        state_dir
            .join("operations")
            .join(format!("{}.json", record.id)),
        serde_json::to_vec(&record).expect("serialize journal fixture"),
    )
    .expect("write unfinished journal");
    std::fs::write(state_dir.join("operations").join("broken.json"), b"{broken")
        .expect("write malformed journal");

    let check = check_operation_journal(&state_dir, &std::collections::BTreeSet::new());
    assert_eq!(check.status, "warn");
    assert!(check.detail.contains("unfinished"));
    assert!(check.detail.contains("malformed"));
    std::fs::remove_dir_all(state_dir).expect("remove journal fixture");
}

#[test]
fn operation_journal_check_does_not_report_a_running_operation() {
    let state_dir = std::env::temp_dir().join(format!(
        "enclave-doctor-journal-running-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(state_dir.join("operations")).expect("create journal fixture");
    let mut record = crate::operation::OperationRecord::new("workspace.start", "sb/ws");
    record.begin("launch_runtime");
    std::fs::write(
        state_dir
            .join("operations")
            .join(format!("{}.json", record.id)),
        serde_json::to_vec(&record).expect("serialize journal fixture"),
    )
    .expect("write open journal");

    // The record is open, but the daemon says that operation is running right
    // now, so it is expected rather than a finding.
    let running = std::collections::BTreeSet::from([record.id.clone()]);
    let check = check_operation_journal(&state_dir, &running);
    assert_eq!(check.status, "ok", "{}", check.detail);

    // With no daemon to vouch for it, the same record is an interrupted
    // operation and is reported.
    let check = check_operation_journal(&state_dir, &std::collections::BTreeSet::new());
    assert_eq!(check.status, "warn");
    assert!(check.detail.contains(&record.id), "{}", check.detail);
    std::fs::remove_dir_all(state_dir).expect("remove journal fixture");
}
