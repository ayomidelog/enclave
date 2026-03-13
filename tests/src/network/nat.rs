use super::*;

#[test]
fn detect_iptables_does_not_panic() {
    let _ = detect_iptables();
}

#[test]
fn anti_spoof_rule_uses_interface_and_assigned_ip() {
    let rule = anti_spoof_rule_args("veth-encl10", "10.200.0.10");
    assert_eq!(
        rule,
        vec![
            "-i",
            "veth-encl10",
            "!",
            "-s",
            "10.200.0.10/32",
            "-j",
            "DROP"
        ]
    );
}

#[test]
fn metadata_block_cidr_is_link_local_metadata_endpoint() {
    assert_eq!(METADATA_IPV4_CIDR, "169.254.169.254/32");
}
