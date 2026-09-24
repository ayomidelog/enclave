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

    let check = check_operation_journal(&state_dir);
    assert_eq!(check.status, "warn");
    assert!(check.detail.contains("unfinished"));
    assert!(check.detail.contains("malformed"));
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
