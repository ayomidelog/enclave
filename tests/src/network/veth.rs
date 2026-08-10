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
fn temporary_peer_name_within_ifnamsiz() {
    for octet in 10..=254u8 {
        let (host, _) = veth_names(octet, "workspace-1");
        assert!(temporary_peer_name(&host).len() <= 15);
    }
}

#[test]
fn workspace_ids_produce_distinct_host_names() {
    let (first, _) = veth_names(10, "workspace-1");
    let (second, _) = veth_names(10, "workspace-2");
    assert_ne!(first, second);
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
