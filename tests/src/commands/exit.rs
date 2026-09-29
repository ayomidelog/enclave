use super::*;

#[test]
fn a_failure_that_named_its_outcome_keeps_its_code() {
    let error = CliExit::error(EXIT_TOKEN_EXISTS, "already stored");
    assert_eq!(exit_code_of(&error), EXIT_TOKEN_EXISTS);
}

#[test]
fn the_code_survives_the_layers_that_only_forward_the_error() {
    let error = CliExit::error(EXIT_INVALID_INPUT, "bad provider").context("while storing a token");
    assert_eq!(exit_code_of(&error), EXIT_INVALID_INPUT);
    // The message is still the wrapped one, so the status did not cost the
    // operator the context that says what was being attempted.
    assert!(format!("{error:#}").contains("while storing a token"));
}

#[test]
fn a_plain_failure_is_a_plain_failure() {
    let error = anyhow::anyhow!("something else went wrong");
    assert_eq!(exit_code_of(&error), EXIT_FAILURE);
}
