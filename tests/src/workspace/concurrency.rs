use super::parse_max_concurrent_commands_for_test;

#[test]
fn an_unset_or_disabled_per_workspace_limit_is_unlimited() {
    assert_eq!(parse_max_concurrent_commands_for_test(None), None);
    assert_eq!(parse_max_concurrent_commands_for_test(Some("")), None);
    assert_eq!(parse_max_concurrent_commands_for_test(Some("0")), None);
    assert_eq!(
        parse_max_concurrent_commands_for_test(Some("unlimited")),
        None
    );
    assert_eq!(
        parse_max_concurrent_commands_for_test(Some("UNLIMITED")),
        None
    );
}

#[test]
fn a_positive_per_workspace_limit_is_parsed() {
    assert_eq!(parse_max_concurrent_commands_for_test(Some("1")), Some(1));
    assert_eq!(parse_max_concurrent_commands_for_test(Some("8")), Some(8));
}

#[test]
fn a_malformed_per_workspace_limit_falls_back_to_unlimited() {
    assert_eq!(parse_max_concurrent_commands_for_test(Some("many")), None);
    assert_eq!(parse_max_concurrent_commands_for_test(Some("-1")), None);
}
