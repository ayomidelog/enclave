use super::*;

/// An empty limit means the workspace did not configure one, and a value that
/// cannot be read is treated the same way rather than failing a start over a
/// secondary limit.
#[test]
fn limits_are_read_from_the_launcher_text() {
    assert_eq!(parse_limit("", "max processes"), None);
    assert_eq!(parse_limit("24", "max processes"), Some(24));
    assert_eq!(parse_limit("0", "max processes"), Some(0));
    assert_eq!(parse_limit("not-a-number", "max processes"), None);
    assert_eq!(parse_limit("-1", "max processes"), None);
}

/// An unnamed workspace gets a fixed placeholder rather than inheriting the
/// host's name, which is what keeps two workspaces from looking like the host.
#[test]
fn an_unnamed_workspace_gets_a_placeholder_hostname() {
    assert_eq!(hostname_to_apply(""), Some("workspace"));
}

/// A hostname the kernel would reject is refused instead of being truncated.
#[test]
fn an_oversized_hostname_is_refused_rather_than_truncated() {
    let too_long = "a".repeat(HOSTNAME_LIMIT);
    assert_eq!(hostname_to_apply(&too_long), None);

    let longest = "a".repeat(HOSTNAME_LIMIT - 1);
    assert_eq!(hostname_to_apply(&longest), Some(longest.as_str()));

    // The workspace name is already lowercased and dash-separated before it gets
    // here, so a realistic name is comfortably inside the limit.
    assert_eq!(
        hostname_to_apply("telegram--worker1"),
        Some("telegram--worker1")
    );
}
