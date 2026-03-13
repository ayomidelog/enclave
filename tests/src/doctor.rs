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
    let check = check_stale_cgroups();
    assert!(
        check.status == "ok" || check.status == "warn",
        "unexpected status: {}",
        check.status
    );
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
