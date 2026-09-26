use super::*;

#[test]
fn veth_names_format() {
    let (host, peer) = veth_names(10, "workspace-1");
    assert_eq!(host, "veth-10-f0f01a");
    assert_eq!(peer, "eth0");
}

#[test]
fn veth_host_name_within_ifnamsiz() {
    for octet in 10..=254u8 {
        let (host, _) = veth_names(octet, "workspace-1");
        assert!(host.len() <= 15, "veth name '{}' exceeds IFNAMSIZ", host);
    }
}

#[test]
fn workspace_ids_produce_distinct_host_names() {
    let (first, _) = veth_names(10, "workspace-1");
    let (second, _) = veth_names(10, "workspace-2");
    assert_ne!(first, second);
}

/// The host side of the pair is built by one ip process, including port
/// isolation, so a workspace start does not pay a second spawn for the bridge
/// utility. Losing the isolation line would silently allow workspace-to-workspace
/// traffic on the bridge, so it is pinned here.
///
/// The peer is created inside the target namespace in the same command. That is
/// worth pinning for two reasons: it is about 40 ms of the start, and a peer created
/// here and moved afterwards is the shape this replaced, so a regression would look
/// like the old code rather than like a bug.
#[test]
fn host_veth_batch_creates_the_peer_in_the_namespace_with_isolation() {
    let batch = host_veth_batch("veth-10-f0f01a", "eth0", 4321);
    let commands: Vec<&str> = batch.lines().map(str::trim).collect();
    assert_eq!(
        commands,
        vec![
            "link add veth-10-f0f01a type veth peer name eth0 netns 4321",
            "link set veth-10-f0f01a master enclave0",
            "link set veth-10-f0f01a type bridge_slave isolated on",
            "link set veth-10-f0f01a up",
        ]
    );
}

#[test]
fn default_route_output_requires_gateway_and_interface_match() {
    assert!(default_route_output_has_route(
        "default via 10.200.0.1 dev eth0 proto static\n",
        "eth0",
        "10.200.0.1"
    ));
    assert!(!default_route_output_has_route(
        "default via 10.200.0.1 dev lo\n",
        "eth0",
        "10.200.0.1"
    ));
    assert!(!default_route_output_has_route(
        "default via 10.200.0.254 dev eth0\n",
        "eth0",
        "10.200.0.1"
    ));
}

/// The octet an Enclave interface name carries is readable back out of it.
///
/// The name is the only record of an address that a second daemon can see: the
/// address itself is inside a network namespace, and the other daemon's registry is
/// not this one's to read. The allocator uses this to refuse an octet another
/// daemon's workspace is already holding, so the round trip has to be exact and a
/// name that is not Enclave's has to yield nothing rather than a guess.
#[test]
fn an_enclave_interface_name_yields_the_octet_it_carries() {
    for octet in [10u8, 12, 100, 254] {
        let (host, _) = veth_names(octet, "workspace-1");
        assert_eq!(
            octet_from_veth_name(&host),
            Some(octet),
            "{host} carries {octet}"
        );
    }

    // A foreign interface is not an Enclave one, whatever it looks like: the peer
    // end is `eth0` inside the workspace and never appears on the host, a name
    // without the hash is not this scheme, and a name from another tool must not be
    // read as an address.
    for foreign in [
        "eth0",
        "veth0",
        "veth-12",
        "veth-12-nothex",
        "veth-abc-123456",
        "veth-1234-123456",
        "docker0",
        "br-abcdef123456",
        "",
    ] {
        assert_eq!(octet_from_veth_name(foreign), None, "{foreign}");
    }
}

/// The hash in a host interface name attributes the interface to a workspace.
///
/// This is what lets a start tell its own leftover from an interface another daemon
/// built: the hash is a function of the workspace id, so it is readable out of the
/// name, and the octet is deliberately not compared because a leftover carries an
/// address no record names any more.
#[test]
fn the_hash_in_a_name_attributes_it_to_its_workspace() {
    for workspace in ["workspace-1", "dev-5d57a02b5f7e", "session-abc-def"] {
        let (host, _) = veth_names(22, workspace);
        assert_eq!(
            hash_from_name(&host),
            Some(workspace_hash(workspace).as_str())
        );
        // The same workspace under a different address is still the same workspace:
        // that is the case a leftover is, so the octet must not be part of the
        // attribution.
        let (other, _) = veth_names(40, workspace);
        assert_eq!(hash_from_name(&other), hash_from_name(&host));
    }

    let (mine, _) = veth_names(22, "workspace-1");
    let (theirs, _) = veth_names(22, "workspace-2");
    assert_ne!(hash_from_name(&mine), hash_from_name(&theirs));
}

#[test]
fn a_name_that_is_not_enclaves_carries_no_hash() {
    for foreign in [
        "eth0",
        "veth0",
        "veth-12",
        "veth-12-nothex",
        "veth-abc-123456",
        "veth-1234-123456",
        "veth-12-12345",
        "veth-12-1234567",
        "docker0",
        "",
    ] {
        assert_eq!(hash_from_name(foreign), None, "{foreign}");
    }
}
