use super::*;

#[test]
fn doctor_check_constructors() {
    let ok = DoctorCheck::ok("test", "all good");
    assert_eq!(ok.status, "ok");
    assert_eq!(ok.name, "test");

    let warn = DoctorCheck::warn("test2", "something wrong");
    assert_eq!(warn.status, "warn");
    assert_eq!(warn.name, "test2");
}

#[test]
fn doctor_report_default_is_empty() {
    let report = DoctorReport::default();
    assert!(report.checks.is_empty());
    assert!(report.status.is_empty());
}

#[test]
fn check_cgroup_v2_does_not_panic() {
    let check = check_cgroup_v2_availability();
    assert!(check.status == "ok" || check.status == "warn");
}

#[test]
fn doctor_check_ok_detail_is_preserved() {
    let detail = "registry is consistent with disk state";
    let check = DoctorCheck::ok("registry_consistency", detail);
    assert_eq!(check.detail, detail);
    assert_eq!(check.name, "registry_consistency");
}

#[test]
fn doctor_check_warn_detail_is_preserved() {
    let detail = "3 stale cgroup(s) found: enclave-ws-1, enclave-ws-2, enclave-ws-3";
    let check = DoctorCheck::warn("stale_cgroups", detail);
    assert_eq!(check.detail, detail);
    assert_eq!(check.status, "warn");
}

#[test]
fn doctor_report_serializes_to_json() {
    let report = DoctorReport {
        status: "healthy".to_string(),
        checks: vec![
            DoctorCheck::ok("check_a", "all good"),
            DoctorCheck::warn("check_b", "minor issue"),
        ],
    };
    let json = serde_json::to_string(&report).expect("serialize");
    assert!(json.contains("healthy"));
    assert!(json.contains("check_a"));
    assert!(json.contains("check_b"));
}

#[test]
fn doctor_report_deserializes_from_json() {
    let json = r#"{"status":"healthy","checks":[{"name":"test","status":"ok","detail":"fine"}]}"#;
    let report: DoctorReport = serde_json::from_str(json).expect("deserialize");
    assert_eq!(report.status, "healthy");
    assert_eq!(report.checks.len(), 1);
    assert_eq!(report.checks[0].name, "test");
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

#[test]
fn doctor_report_all_ok_is_healthy() {
    let checks = [DoctorCheck::ok("a", "good"), DoctorCheck::ok("b", "good")];
    let all_ok = checks.iter().all(|c| c.status == "ok");
    assert!(all_ok);
}

#[test]
fn doctor_report_any_warn_is_issues_detected() {
    let checks = [DoctorCheck::ok("a", "good"), DoctorCheck::warn("b", "bad")];
    let all_ok = checks.iter().all(|c| c.status == "ok");
    assert!(!all_ok);
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
fn orphaned_mount_check_counts_enclave_leftovers_and_names_foreign_mounts() {
    let state_dir = Path::new("/srv/enclave");
    let sandboxes = state_dir.join("sandboxes");
    // One Enclave overlay left behind, one Enclave overlay still in use by a
    // running workspace, and one tmpfs the operator placed under a workspace.
    let snapshot = crate::fsutil::MountInfoSnapshot::parse(concat!(
        "100 1 0:50 / /srv/enclave/sandboxes/sb/workspaces/gone/home-merged rw - overlay overlay rw\n",
        "101 1 0:51 / /srv/enclave/sandboxes/sb/workspaces/live/home-merged rw - overlay overlay rw\n",
        "102 1 0:60 / /srv/enclave/sandboxes/sb/workspaces/live/backup rw - tmpfs tmpfs rw\n"
    ));
    let active_roots = vec![PathBuf::from("/srv/enclave/sandboxes/sb/workspaces/live")];

    let (orphaned, foreign) = classify_sandbox_mounts(&snapshot, &sandboxes, &active_roots);
    assert_eq!(
        orphaned.len(),
        1,
        "only the leftover overlay is orphaned: {orphaned:?}"
    );
    assert!(orphaned[0].contains("gone/home-merged"), "{orphaned:?}");
    assert_eq!(foreign.len(), 1, "the tmpfs is foreign: {foreign:?}");
    assert!(
        foreign[0].contains("backup") && foreign[0].contains("source tmpfs"),
        "{foreign:?}"
    );
}

#[test]
fn a_foreign_mount_under_a_workspace_is_not_reported_as_an_enclave_leftover() {
    let state_dir = Path::new("/srv/enclave");
    let sandboxes = state_dir.join("sandboxes");
    let snapshot = crate::fsutil::MountInfoSnapshot::parse(concat!(
        "200 1 8:1 / /srv/enclave/sandboxes/sb/workspaces/ws/backup rw - ext4 /dev/sda1 rw\n"
    ));

    let (orphaned, foreign) = classify_sandbox_mounts(&snapshot, &sandboxes, &[]);
    assert!(
        orphaned.is_empty(),
        "a mount Enclave did not create is not an Enclave leftover: {orphaned:?}"
    );
    assert_eq!(foreign.len(), 1, "{foreign:?}");
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
fn repair_removes_orphaned_sandbox_directories_and_validates_daemon_state() {
    let temp_dir = std::env::temp_dir().join(format!(
        "enclave-doctor-repair-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let socket_path = temp_dir.join("daemon.sock");
    std::fs::create_dir_all(temp_dir.join("sandboxes").join("orphan")).unwrap();

    let state_lock = crate::daemon::state_lock::acquire_state_lock(&temp_dir, &socket_path)
        .expect("acquire daemon state lock");
    let report = repair_doctor(&temp_dir, &socket_path).expect("repair state");

    assert!(report.daemon_state_consistent);
    assert!(!temp_dir.join("sandboxes").join("orphan").exists());

    drop(state_lock);
    let _ = std::fs::remove_dir_all(&temp_dir);
}
