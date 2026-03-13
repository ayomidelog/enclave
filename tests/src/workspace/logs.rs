use super::sanitize_log_header_field;

#[test]
fn header_fields_escape_control_characters() {
    assert_eq!(
        sanitize_log_header_field("cmd\n--flag\tvalue"),
        "cmd\\n--flag\\tvalue"
    );
}
