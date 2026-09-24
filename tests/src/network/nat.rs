use super::*;

#[test]
fn detect_iptables_does_not_panic() {
    let _ = detect_iptables();
}

#[test]
fn anti_spoof_rule_uses_interface_and_assigned_ip() {
    let rule = anti_spoof_rule_args("veth-encl10", "10.200.0.10", None);
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
fn anti_spoof_rule_tags_enclave_ownership() {
    let rule = anti_spoof_rule_args("veth-encl10", "10.200.0.10", Some("session-abc"));
    assert_eq!(
        rule,
        vec![
            "-i",
            "veth-encl10",
            "!",
            "-s",
            "10.200.0.10/32",
            "-m",
            "comment",
            "--comment",
            "enclave:session-abc",
            "-j",
            "DROP"
        ]
    );
}

#[test]
fn owned_rules_are_parsed_from_iptables_save_output() {
    let output = "-P INPUT ACCEPT\n\
-A INPUT -i veth-3-0a1b2c ! -s 10.200.0.3/32 -m comment --comment \"enclave:session-abc\" -j DROP\n\
-A FORWARD -i veth-3-0a1b2c ! -s 10.200.0.3/32 -m comment --comment enclave:session-abc -j DROP\n\
-A INPUT -s 10.1.2.3/32 -j ACCEPT\n\
-A FORWARD -m comment --comment \"unrelated\" -j ACCEPT\n";
    let rules = parse_owned_rules(output);
    assert_eq!(rules.len(), 2);
    assert_eq!(rules[0].chain, "INPUT");
    assert_eq!(rules[0].owner, "session-abc");
    assert_eq!(rules[1].chain, "FORWARD");
    assert_eq!(rules[1].owner, "session-abc");
}

#[test]
fn metadata_block_cidr_is_link_local_metadata_endpoint() {
    assert_eq!(METADATA_IPV4_CIDR, "169.254.169.254/32");
}
