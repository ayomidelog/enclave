//! The confirmation a destructive command requires, and what an unanswered
//! prompt means.
//!
//! These are the outcomes a caller cannot see for itself: a decline and a prompt
//! that could not be read both print nothing to standard output, and only one of
//! them is a failure.

use super::*;

/// A script with no input reads end of file, which is not an answer.
#[test]
fn end_of_file_is_not_an_answer() {
    assert!(matches!(interpret_answer(0, "", "y"), Answer::Unanswerable));
}

#[test]
fn the_required_line_matches() {
    assert!(matches!(interpret_answer(2, "y\n", "y"), Answer::Matched));
    assert!(matches!(
        interpret_answer(19, "delete all sandboxes\n", "delete all sandboxes"),
        Answer::Matched
    ));
}

#[test]
fn any_other_line_declines() {
    for line in ["n\n", "yes\n", "Y\n", "\n", "delete all sandbox\n"] {
        assert!(
            matches!(
                interpret_answer(line.len(), line, "delete all sandboxes"),
                Answer::Declined
            ),
            "{line:?} must not confirm"
        );
    }
}

/// Surrounding whitespace is the operator's, not a different answer.
#[test]
fn surrounding_whitespace_does_not_change_the_answer() {
    assert!(matches!(
        interpret_answer(4, "  y  \n", "y"),
        Answer::Matched
    ));
}

/// A prompt that could not be answered is an error, so a script cannot take an
/// exit status of zero to mean the teardown happened.
#[test]
fn an_unanswerable_prompt_fails_the_command_and_names_a_way_forward() {
    let error = require_confirmation(
        Confirmation::Unanswerable,
        "wipe",
        "Run it from a terminal.",
    )
    .expect_err("an unanswered prompt must fail");
    let message = format!("{error:#}");
    assert!(message.contains("wipe"), "{message}");
    assert!(message.contains("not a terminal"), "{message}");
    assert!(message.contains("nothing was deleted"), "{message}");
    assert!(message.contains("Run it from a terminal."), "{message}");
}

/// The operator saying no is the operator getting what they asked for.
#[test]
fn a_decline_succeeds_without_acting() {
    require_confirmation(Confirmation::Declined, "wipe", "unused")
        .expect("a decline is a normal outcome");
}

#[test]
fn a_confirmed_prompt_proceeds() {
    require_confirmation(Confirmation::Confirmed, "wipe", "unused")
        .expect("a confirmed prompt proceeds");
}
