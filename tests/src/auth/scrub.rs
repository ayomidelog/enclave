use super::*;

fn secrets(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}

#[test]
fn every_occurrence_of_a_secret_is_replaced() {
    let output = scrub_secrets("token=abc123 and again abc123", &secrets(&["abc123"]));
    assert_eq!(output, "token=[REDACTED] and again [REDACTED]");
}

#[test]
fn the_longest_secret_wins() {
    // Replacing the shorter one first would leave "def" behind.
    let output = scrub_secrets("value abcdef here", &secrets(&["abc", "abcdef"]));
    assert_eq!(output, "value [REDACTED] here");
}

#[test]
fn several_secrets_are_all_replaced() {
    let output = scrub_secrets("one=aaa two=bbb", &secrets(&["aaa", "bbb"]));
    assert_eq!(output, "one=[REDACTED] two=[REDACTED]");
}

#[test]
fn output_without_a_secret_is_unchanged() {
    let text = "nothing to hide here";
    assert_eq!(scrub_secrets(text, &secrets(&["abc123"])), text);
}

#[test]
fn an_empty_secret_does_not_redact_everything() {
    let text = "plain output";
    assert_eq!(scrub_secrets(text, &secrets(&[""])), text);
}

#[test]
fn text_that_is_not_ascii_survives_a_scrub() {
    let output = scrub_secrets("héllo 🔒 token abc123", &secrets(&["abc123"]));
    assert_eq!(output, "héllo 🔒 token [REDACTED]");
}

#[test]
fn a_secret_at_the_very_start_and_end_is_replaced() {
    let output = scrub_secrets("abc123 middle abc123", &secrets(&["abc123"]));
    assert_eq!(output, "[REDACTED] middle [REDACTED]");
}
