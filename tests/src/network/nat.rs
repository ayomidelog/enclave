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

#[test]
fn anti_spoof_chains_are_detected_for_both_rule_shapes() {
    let tagged = "-P INPUT ACCEPT\n\
-A INPUT -i veth-4-0a1b2c ! -s 10.200.0.4/32 -m comment --comment \"enclave:session-a\" -j DROP\n\
-A FORWARD -i veth-4-0a1b2c ! -s 10.200.0.4/32 -j DROP\n";
    assert_eq!(
        chains_with_anti_spoof_rule(tagged, "veth-4-0a1b2c", "10.200.0.4"),
        vec!["INPUT", "FORWARD"]
    );
}

/// Another workspace's interface must never be reported as this workspace's
/// leftover rule, and an unrelated rule on the same interface must not be
/// mistaken for the anti-spoofing rule either.
#[test]
fn anti_spoof_detection_is_scoped_to_the_interface_and_address() {
    let other = "-A INPUT -i veth-5-0a1b2c ! -s 10.200.0.5/32 -j DROP\n\
-A INPUT -i veth-4-0a1b2c -j ACCEPT\n";
    assert!(chains_with_anti_spoof_rule(other, "veth-4-0a1b2c", "10.200.0.4").is_empty());
}

#[test]
fn anti_spoof_detection_ignores_other_chains() {
    let other_chain = "-A DOCKER-USER -i veth-4-0a1b2c ! -s 10.200.0.4/32 -j DROP\n";
    assert!(chains_with_anti_spoof_rule(other_chain, "veth-4-0a1b2c", "10.200.0.4").is_empty());
}
