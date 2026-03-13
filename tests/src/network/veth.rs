use super::*;

#[test]
fn veth_names_format() {
    let (host, peer) = veth_names(10);
    assert_eq!(host, "veth-encl10");
    assert_eq!(peer, "eth0");
}

#[test]
fn veth_host_name_within_ifnamsiz() {
    for octet in 10..=254u8 {
        let (host, _) = veth_names(octet);
        assert!(host.len() <= 15, "veth name '{}' exceeds IFNAMSIZ", host);
    }
}
