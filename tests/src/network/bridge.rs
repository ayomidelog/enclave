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
#[test]
fn parse_subnet_conflicts_names_other_interfaces_on_the_enclave_subnet() {
    let output = "\
1: lo    inet 127.0.0.1/8 scope host lo\\       valid_lft forever preferred_lft forever\n\
2: eth0    inet 81.17.99.208/22 brd 81.17.99.255 scope global eth0\\       valid_lft forever\n\
3: docker0    inet 10.200.0.5/24 brd 10.200.0.255 scope global docker0\\       valid_lft forever\n\
4: enclave0    inet 10.200.0.1/24 scope global enclave0\\       valid_lft forever\n";
    assert_eq!(
        parse_subnet_conflicts(output),
        vec!["docker0 (10.200.0.5/24)".to_string()]
    );
}

#[test]
fn parse_subnet_conflicts_ignores_the_bridge_and_neighbouring_subnets() {
    let output = "\
2: eth0    inet 10.201.0.1/24 scope global eth0\\       valid_lft forever\n\
3: enclave0    inet 10.200.0.1/24 scope global enclave0\\       valid_lft forever\n";
    assert!(parse_subnet_conflicts(output).is_empty());
}

#[test]
fn parse_ipv4_addresses_by_interface_pairs_each_address_with_its_interface() {
    let output = "\
3: docker0    inet 10.200.0.5/24 brd 10.200.0.255 scope global docker0\\       valid_lft forever\n\
4: enclave0    inet6 fe80::1/64 scope link\n";
    assert_eq!(
        parse_ipv4_addresses_by_interface(output),
        vec![("docker0".to_string(), "10.200.0.5/24".to_string())]
    );
}
