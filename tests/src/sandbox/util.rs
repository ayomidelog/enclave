use super::*;

#[test]
fn validate_name_rejects_dot_segments() {
    assert!(validate_name("devbox").is_ok());
    assert!(validate_name("name.with.dot").is_err());
    assert!(validate_name(".hidden").is_err());
}

#[test]
fn validate_debootstrap_inputs_rejects_untrusted_inputs() {
    assert!(validate_debootstrap_inputs("bookworm", "http://deb.debian.org/debian").is_ok());
    assert!(validate_debootstrap_inputs("bookworm", "https://").is_err());
    assert!(validate_debootstrap_inputs("bookworm", "file:///tmp/mirror").is_err());
    assert!(validate_debootstrap_inputs("bookworm", "http:// bad").is_err());
    assert!(validate_debootstrap_inputs("../bookworm", "http://deb.debian.org/debian").is_err());
}
