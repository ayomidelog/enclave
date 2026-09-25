use super::*;

use serde_json::json;

#[test]
fn a_transition_is_added_to_an_object_result() {
    let result = with_transition(json!({"id": "w1"}), "stopped", "running");
    assert_eq!(result["previous_state"], "stopped");
    assert_eq!(result["current_state"], "running");
    assert_eq!(result["id"], "w1");
}

#[test]
fn a_transition_leaves_a_non_object_result_alone() {
    // The lifecycle operations all answer with an object. A result that is not one
    // must not be replaced by a report about it.
    let result = with_transition(json!([1, 2, 3]), "stopped", "running");
    assert_eq!(result, json!([1, 2, 3]));
}

#[test]
fn a_state_that_cannot_be_read_is_reported_as_unknown() {
    let state_dir =
        std::env::temp_dir().join(format!("enclave-transition-unknown-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&state_dir);
    std::fs::create_dir_all(&state_dir).expect("create state dir");
    crate::registry::ensure_registry(&state_dir).expect("create registry");

    // The registry loaded, but neither target exists in it. Both reads report
    // unknown rather than absent: absent is reserved for a state the operation
    // itself removed, and a read that cannot find a target has no such evidence.
    assert_eq!(
        workspace_state_before(&state_dir, "missing", "missing"),
        UNKNOWN
    );
    assert_eq!(sandbox_state_before(&state_dir, "missing"), UNKNOWN);

    let _ = std::fs::remove_dir_all(state_dir);
}
