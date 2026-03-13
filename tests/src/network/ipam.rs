use super::*;

#[test]
fn allocate_first_ip() {
    let used = BTreeSet::new();
    let ip = allocate_ip(&used).unwrap();
    assert_eq!(ip, "10.200.0.10");
}

#[test]
fn allocate_skips_used() {
    let mut used = BTreeSet::new();
    used.insert(10);
    used.insert(11);
    let ip = allocate_ip(&used).unwrap();
    assert_eq!(ip, "10.200.0.12");
}

#[test]
fn allocate_exhausted() {
    let used: BTreeSet<u8> = (POOL_START..=POOL_END).collect();
    let result = allocate_ip(&used);
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("exhausted"), "got: {msg}");
}

#[test]
fn parse_host_octet_valid() {
    assert_eq!(parse_host_octet("10.200.0.42"), Some(42));
    assert_eq!(parse_host_octet("10.200.0.1"), Some(1));
}

#[test]
fn parse_host_octet_wrong_subnet() {
    assert_eq!(parse_host_octet("192.168.1.10"), None);
}

#[test]
fn parse_host_octet_invalid() {
    assert_eq!(parse_host_octet("not-an-ip"), None);
}

#[test]
fn gateway_is_in_subnet() {
    assert_eq!(parse_host_octet(GATEWAY_IP), Some(1));
}
