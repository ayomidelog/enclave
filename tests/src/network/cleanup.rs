use super::{teardown_workspace_network, NetworkCleanupFailure, NetworkCleanupReport};

#[test]
fn invalid_workspace_ip_produces_an_incomplete_cleanup_report() {
    let report = teardown_workspace_network("not-an-enclave-ip", "ws-1");
    assert!(!report.is_complete());
    assert_eq!(report.workspace_id, "ws-1");
    assert_eq!(report.failures[0].resource, "workspace-ip");
}

#[test]
fn network_cleanup_report_requires_all_resources_to_be_absent() {
    let report = NetworkCleanupReport {
        workspace_id: "ws-1".to_string(),
        assigned_ip: "10.200.0.10".to_string(),
        veth_host: Some("veth-10-abcdef".to_string()),
        anti_spoof_rules_absent: true,
        veth_absent: true,
        failures: Vec::new(),
    };
    assert!(report.is_complete());
}

#[test]
fn network_cleanup_report_preserves_each_failed_resource() {
    let report = NetworkCleanupReport {
        workspace_id: "ws-1".to_string(),
        assigned_ip: "10.200.0.10".to_string(),
        veth_host: Some("veth-10-abcdef".to_string()),
        anti_spoof_rules_absent: false,
        veth_absent: true,
        failures: vec![NetworkCleanupFailure {
            resource: "anti-spoofing-rules".to_string(),
            message: "iptables denied deletion".to_string(),
        }],
    };
    assert!(!report.is_complete());
    assert_eq!(report.failures[0].resource, "anti-spoofing-rules");
}
