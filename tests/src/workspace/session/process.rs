use super::*;

#[test]
fn hostname_lowercases_name() {
    assert_eq!(workspace_runtime_hostname("MyProject"), "myproject");
}

#[test]
fn hostname_replaces_special_chars_with_dash() {
    assert_eq!(
        workspace_runtime_hostname("my project!here"),
        "my-project-here"
    );
}

#[test]
fn hostname_collapses_consecutive_dashes() {
    assert_eq!(workspace_runtime_hostname("hello---world"), "hello-world");
}

#[test]
fn hostname_trims_leading_trailing_dashes() {
    assert_eq!(workspace_runtime_hostname("--hello--"), "hello");
}

#[test]
fn hostname_truncates_to_63_chars() {
    let long_name = "a".repeat(100);
    let result = workspace_runtime_hostname(&long_name);
    assert!(result.len() <= 63);
}

#[test]
fn hostname_empty_name_returns_workspace() {
    assert_eq!(workspace_runtime_hostname(""), "workspace");
}

#[test]
fn hostname_all_special_returns_workspace() {
    assert_eq!(workspace_runtime_hostname("!!!"), "workspace");
}

#[test]
fn process_alive_returns_true_for_self() {
    assert!(process_alive(std::process::id()));
}

#[test]
fn process_alive_returns_false_for_impossible_pid() {
    assert!(!process_alive(u32::MAX));
}

#[test]
fn process_matches_returns_false_for_dead_process() {
    assert!(!process_matches(u32::MAX, None));
}

#[test]
fn starttime_ticks_succeeds_for_self() {
    let result = process_starttime_ticks(std::process::id());
    assert!(result.is_ok(), "expected Ok, got: {:?}", result);
}

#[test]
fn parse_status_kb_value_extracts_number() {
    assert_eq!(parse_status_kb_value("   1234 kB"), Some(1234));
}

#[test]
fn parse_status_kb_value_returns_none_for_empty() {
    assert_eq!(parse_status_kb_value(""), None);
}

#[test]
fn parse_status_kb_value_returns_none_for_non_numeric() {
    assert_eq!(parse_status_kb_value("abc kB"), None);
}

#[test]
fn runtime_cmdline_detection_accepts_bootstrap_helper() {
    assert!(looks_like_enclave_runtime_cmdline(
        "enclave internal workspace-session-bootstrap --rootfs /tmp/rootfs --ready-file /tmp/ready"
    ));
}

#[test]
fn runtime_cmdline_detection_rejects_unrelated_process() {
    assert!(!looks_like_enclave_runtime_cmdline("/usr/bin/bash -lc env"));
}
