use super::{is_retryable_delete_error, veth_is_present};

#[test]
fn veth_presence_is_read_from_the_net_class_directory() {
    // The loopback interface is always present; the deleted case is what a
    // teardown has to treat as success, and it is decided by this lookup rather
    // than by ip's wording.
    assert!(veth_is_present("lo"));
    assert!(!veth_is_present("veth-enclave-missing-test-interface"));
}

#[test]
fn transient_veth_delete_errors_are_retryable() {
    assert!(is_retryable_delete_error(
        "RTNETLINK answers: Device or resource busy"
    ));
    assert!(is_retryable_delete_error("temporarily unavailable"));
    assert!(is_retryable_delete_error("operation in progress"));
}

#[test]
fn permanent_veth_delete_errors_are_not_retried() {
    assert!(!is_retryable_delete_error("Operation not permitted"));
    assert!(!is_retryable_delete_error("invalid argument"));
    assert!(!is_retryable_delete_error("Cannot find device"));
}
