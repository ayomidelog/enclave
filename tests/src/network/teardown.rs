use super::link_is_already_absent;

#[test]
fn missing_veth_is_treated_as_already_cleaned() {
    for message in [
        b"Cannot find device \"veth-10-123abc\"".as_slice(),
        b"Device veth-10-123abc does not exist",
        b"No such device",
    ] {
        assert!(link_is_already_absent(message));
    }
}

#[test]
fn other_veth_delete_errors_are_preserved() {
    assert!(!link_is_already_absent(b"Operation not permitted"));
    assert!(!link_is_already_absent(
        b"RTNETLINK answers: Device or resource busy"
    ));
}
