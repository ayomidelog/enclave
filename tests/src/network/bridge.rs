use super::*;

#[test]
fn parse_ipv4_addresses_extracts_only_inet_fields() {
    let output = "2: enclave0: <BROADCAST>\n    inet 10.200.0.1/24 scope global enclave0\n    inet6 fe80::1/64 scope link\n";
    assert_eq!(parse_ipv4_addresses(output), vec!["10.200.0.1/24"]);
}

#[test]
fn parse_ipv4_addresses_keeps_multiple_addresses_for_collision_check() {
    let output = "    inet 192.0.2.1/24 scope global\n    inet 10.200.0.5/24 scope global\n";
    assert_eq!(
        parse_ipv4_addresses(output),
        vec!["192.0.2.1/24", "10.200.0.5/24"]
    );
}
