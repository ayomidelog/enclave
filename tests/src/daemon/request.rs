use super::operation_target;

#[test]
fn a_failed_request_can_be_attributed_to_its_sandbox_and_workspace() {
    assert_eq!(
        operation_target(&serde_json::json!({
            "sandbox": "sandbox-a",
            "workspace": "workspace-b",
        })),
        "sandbox-a/workspace-b"
    );
    assert_eq!(
        operation_target(&serde_json::json!({"sandbox_id": "sandbox-a"})),
        "sandbox-a"
    );
    assert_eq!(
        operation_target(&serde_json::json!({
            "name": "sandbox-by-name",
            "workspace_id": "workspace-b",
        })),
        "sandbox-by-name/workspace-b"
    );
    assert_eq!(operation_target(&serde_json::json!({})), "-");
}
