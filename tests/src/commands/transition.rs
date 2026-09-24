use super::*;

use serde_json::json;

#[test]
fn a_real_change_is_recognized() {
    let response = json!({"previous_state": "running", "current_state": "stopped"});
    assert_eq!(transition_of(&response), Some(("running", "stopped")));
}

#[test]
fn an_unchanged_state_is_recognized_as_such() {
    let response = json!({"previous_state": "stopped", "current_state": "stopped"});
    let (previous, current) = transition_of(&response).expect("a transition pair");
    assert_eq!(previous, current);
}

#[test]
fn a_response_without_a_transition_reports_none() {
    assert_eq!(transition_of(&json!({"id": "w1"})), None);
    // A response with only half the pair is not a transition either.
    assert_eq!(transition_of(&json!({"previous_state": "running"})), None);
}
