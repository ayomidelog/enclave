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
