use super::*;

/// The restore script is parsed by iptables-restore the same way iptables-save
/// writes it, so a value containing whitespace has to be quoted or the parser
/// splits it into two arguments.
#[test]
fn restore_arguments_are_quoted_only_when_they_need_to_be() {
    // Plain values are left alone, which is what keeps the common case readable.
    for plain in [
        "INPUT",
        "-i",
        "veth-10-0a1b2c",
        "10.200.0.10/32",
        "DROP",
        "!",
    ] {
        assert_eq!(quote_restore_argument(plain), plain);
    }

    // A comment with spaces is the realistic case.
    assert_eq!(
        quote_restore_argument("enclave:session-abc"),
        "enclave:session-abc"
    );
    assert_eq!(quote_restore_argument("two words"), "\"two words\"");
    assert_eq!(quote_restore_argument(""), "\"\"");

    // A quote inside the value is escaped rather than ending the quoted run.
    assert_eq!(quote_restore_argument("a\"b"), "\"a\\\"b\"");
}

/// The nft and legacy variants keep separate rule sets, so the restore has to use
/// the binary that matches the detected iptables.
#[test]
fn the_restore_binary_matches_the_detected_iptables() {
    assert_eq!(restore_binary_for("iptables-nft"), "iptables-nft-restore");
    assert_eq!(
        restore_binary_for("iptables-legacy"),
        "iptables-legacy-restore"
    );
    assert_eq!(restore_binary_for("iptables"), "iptables-restore");
}

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

/// `iptables` normalizes a negated match to the front of the rule, so the rule
/// it prints is not the argument list Enclave installed. Matching on the
/// installed order found nothing, which made both the removal and its absence
/// check vacuous: every workspace start leaked two rules and the stop still
/// reported that the network had been released.
#[test]
fn anti_spoof_detection_matches_the_order_iptables_prints() {
    let normalized = "-P INPUT ACCEPT\n\
-A INPUT ! -s 10.200.0.4/32 -i veth-4-0a1b2c -m comment --comment \"enclave:session-a\" -j DROP\n\
-A FORWARD ! -s 10.200.0.4/32 -i veth-4-0a1b2c -m comment --comment \"enclave:session-a\" -j DROP\n";
    assert_eq!(
        chains_with_anti_spoof_rule(normalized, "veth-4-0a1b2c", "10.200.0.4"),
        vec!["INPUT", "FORWARD"]
    );
    // The interface and address alone are not enough: the source must be negated
    // and the target must be DROP.
    let allow_same_interface = "-A INPUT -i veth-4-0a1b2c -j ACCEPT\n";
    assert!(
        chains_with_anti_spoof_rule(allow_same_interface, "veth-4-0a1b2c", "10.200.0.4").is_empty()
    );
    let unnegated_source = "-A INPUT -s 10.200.0.4/32 -i veth-4-0a1b2c -j DROP\n";
    assert!(
        chains_with_anti_spoof_rule(unnegated_source, "veth-4-0a1b2c", "10.200.0.4").is_empty()
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
#[test]
fn split_rule_args_rebuilds_a_quoted_ownership_comment() {
    let rule = "-s 10.200.0.0/24 -m comment --comment \"enclave:bridge\" -j ACCEPT";
    assert_eq!(
        split_rule_args(rule),
        vec![
            "-s",
            "10.200.0.0/24",
            "-m",
            "comment",
            "--comment",
            "enclave:bridge",
            "-j",
            "ACCEPT"
        ]
    );
}

#[test]
fn split_rule_args_keeps_an_unquoted_comment_whole() {
    let rule =
        "-i veth-3-0a1b2c ! -s 10.200.0.3/32 -m comment --comment enclave:session-abc -j DROP";
    assert!(split_rule_args(rule).contains(&"enclave:session-abc".to_string()));
}

#[test]
fn bridge_rules_are_tagged_and_record_the_untagged_shape() {
    let rules = bridge_rules();
    let tag = vec![
        "-m".to_string(),
        "comment".to_string(),
        "--comment".to_string(),
        format!("{RULE_COMMENT_PREFIX}{BRIDGE_RULE_OWNER}"),
    ];
    for rule in &rules {
        // The tagged shape is the untagged one with the ownership comment
        // inserted before the jump target, which is where `iptables -S` prints
        // a match module.
        let mut tagged = rule.legacy_args.clone();
        let target = tagged.split_off(tagged.len() - 2);
        tagged.extend(tag.clone());
        tagged.extend(target);
        assert_eq!(tagged, rule.args, "{} {}", rule.table, rule.chain);
    }
    assert_eq!(rules.iter().filter(|rule| rule.table == "nat").count(), 1);
    assert!(rules.iter().any(|rule| rule.table == "filter"));
}

#[test]
fn bridge_rules_keep_the_match_each_shared_rule_needs() {
    let rules = bridge_rules();
    let has = |table: &str, chain: &str, marker: &str| {
        rules.iter().any(|rule| {
            rule.table == table
                && rule.chain == chain
                && rule.legacy_args.contains(&marker.to_string())
        })
    };
    assert!(has("filter", "INPUT", "addrtype"));
    assert!(has("filter", "FORWARD", "conntrack"));
    assert!(has("filter", "FORWARD", METADATA_IPV4_CIDR));
    assert!(has("nat", "POSTROUTING", "MASQUERADE"));
}

#[test]
fn stale_bridge_rules_retire_other_shapes_and_ignore_workspace_rules() {
    let rules = bridge_rules();
    let current = rules[0].clone();
    let owned = vec![
        OwnedRule {
            table: current.table.to_string(),
            chain: current.chain.to_string(),
            owner: BRIDGE_RULE_OWNER.to_string(),
            rule: current.args.join(" "),
        },
        // A previous release ordered the ownership comment before the match.
        OwnedRule {
            table: "filter".to_string(),
            chain: "FORWARD".to_string(),
            owner: BRIDGE_RULE_OWNER.to_string(),
            rule: "-m comment --comment \"enclave:bridge\" -s 10.200.0.0/24 -j ACCEPT".to_string(),
        },
        OwnedRule {
            table: "filter".to_string(),
            chain: "INPUT".to_string(),
            owner: "session-abc".to_string(),
            rule: "-i veth-3-0a1b2c -j DROP".to_string(),
        },
    ];

    let stale = stale_bridge_rules(&rules, &owned);
    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0].table, "filter");
    assert_eq!(stale[0].chain, "FORWARD");
    // An empty expected set retires every shared rule, which a teardown needs.
    assert_eq!(stale_bridge_rules(&[], &owned).len(), 2);
}
