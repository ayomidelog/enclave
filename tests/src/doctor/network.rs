use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use super::*;
use crate::network::veth::is_enclave_veth_name;

fn veth_name(octet: u8, workspace_id: &str) -> String {
    crate::network::veth::veth_names(octet, workspace_id).0
}

#[test]
fn veth_inventory_flags_leaks_and_missing_interfaces() {
    let owned = veth_name(4, "session-aaaa");
    let stale = veth_name(7, "session-bbbb");
    let mut expected = BTreeMap::new();
    expected.insert(owned.clone(), "session-aaaa".to_string());
    let present = BTreeSet::from([owned.clone(), stale.clone()]);

    let diff = veth_inventory_diff(&expected, &present);
    assert_eq!(diff.leaked_veths, vec![stale]);
    assert!(diff.missing_veths.is_empty());

    let diff = veth_inventory_diff(&expected, &BTreeSet::new());
    assert_eq!(diff.leaked_veths, Vec::<String>::new());
    assert_eq!(diff.missing_veths.len(), 1);
    assert!(diff.missing_veths[0].contains("session-aaaa"));
}

#[test]
fn owned_rule_owners_ignore_unrelated_and_known_workspaces() {
    let expected = BTreeSet::from(["session-aaaa".to_string()]);
    let rules = vec![
        crate::network::nat::OwnedRule {
            chain: "INPUT".to_string(),
            owner: "session-aaaa".to_string(),
            rule: String::new(),
        },
        crate::network::nat::OwnedRule {
            chain: "FORWARD".to_string(),
            owner: "session-bbbb".to_string(),
            rule: String::new(),
        },
    ];

    assert_eq!(leaked_rule_owners(&expected, &rules), vec!["session-bbbb"]);
}

#[test]
fn loop_device_parser_reads_backing_files() {
    let output = "/dev/loop0: [2049]:12345 (/var/lib/enclave/sandboxes/sb/workspaces/ws/fs.img)\n\
/dev/loop1: [2049]:54321 (/var/lib/enclave/sandboxes/sb/workspaces/other/fs.img (deleted))\n\
not a loop line\n";
    let devices = parse_loop_devices(output);
    assert_eq!(devices.len(), 2);
    assert_eq!(devices[0].0, "/dev/loop0");
    assert_eq!(
        devices[0].1,
        PathBuf::from("/var/lib/enclave/sandboxes/sb/workspaces/ws/fs.img")
    );
    assert_eq!(
        devices[1].1,
        PathBuf::from("/var/lib/enclave/sandboxes/sb/workspaces/other/fs.img")
    );
}

#[test]
fn loop_inventory_reports_stopped_and_untracked_images() {
    let state_dir = PathBuf::from("/var/lib/enclave");
    let active = state_dir.join("sandboxes/sb/workspaces/active/fs.img");
    let stopped = state_dir.join("sandboxes/sb/workspaces/stopped/fs.img");
    let foreign = PathBuf::from("/srv/other/disk.img");
    let expected = BTreeMap::from([
        (active.clone(), ("session-active".to_string(), true)),
        (stopped.clone(), ("session-stopped".to_string(), false)),
    ]);
    let devices = vec![
        ("/dev/loop0".to_string(), active),
        ("/dev/loop1".to_string(), stopped),
        ("/dev/loop2".to_string(), foreign),
    ];

    let leaked = leaked_loop_devices(&state_dir, &expected, &devices);
    assert_eq!(leaked.len(), 1);
    assert!(leaked[0].contains("session-stopped"));
}

#[test]
fn enclave_veth_name_recognition_is_strict() {
    assert!(is_enclave_veth_name(&veth_name(4, "session-aaaa")));
    assert!(!is_enclave_veth_name("eth0"));
    assert!(!is_enclave_veth_name("veth-encl10"));
    assert!(!is_enclave_veth_name("veth-9999999-abcdef"));
    assert!(!is_enclave_veth_name("veth-4-zzzzzz"));
}
