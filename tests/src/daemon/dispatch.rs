use super::*;

#[test]
fn action_parse_known_actions() {
    let actions = [
        "ping",
        "daemon.health",
        "daemon.doctor",
        "init",
        "sandbox.create",
        "sandbox.update",
        "sandbox.start",
        "sandbox.stop",
        "sandbox.status",
        "sandbox.destroy",
        "sandbox.list",
        "sandbox.remove",
        "sandbox.exec_setup",
        "process.list",
        "workspace.create",
        "workspace.start",
        "workspace.stop",
        "workspace.destroy",
        "workspace.status",
        "workspace.stats",
        "workspace.stats.list",
        "workspace.list",
        "workspace.remove",
        "workspace.update",
        "workspace.update_auth",
        "workspace.exec",
        "workspace.port.publish",
        "workspace.port.unpublish",
        "workspace.port.list",
        "workspace.runtime",
        "workspace.logs",
        "workspace.snapshot",
        "workspace.snapshot.list",
        "workspace.restore",
        "workspace.snapshot.gc",
        "workspace.snapshot.export",
        "workspace.snapshot.import",
        "registry.repair",
        "policy.get",
        "policy.set_default",
        "policy.allow",
        "policy.deny",
        "policy.clear",
        "shutdown",
    ];
    for action in actions {
        assert!(
            Action::parse(action).is_ok(),
            "expected '{}' to parse as a valid action",
            action
        );
    }
}

#[test]
fn action_parse_rejects_unknown_action() {
    let result = Action::parse("totally.bogus");
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("unknown action"), "got: {}", msg);
}

#[test]
fn require_param_str_finds_first_key() {
    let params = serde_json::json!({
        "sandbox_id": "abc",
        "workspace": "ws1",
    });
    let result = require_param_str(&params, &["sandbox", "sandbox_id"]).unwrap();
    assert_eq!(result, "abc");
}

#[test]
fn require_param_str_prefers_first_match() {
    let params = serde_json::json!({
        "sandbox": "first",
        "sandbox_id": "second",
    });
    let result = require_param_str(&params, &["sandbox", "sandbox_id"]).unwrap();
    assert_eq!(result, "first");
}

#[test]
fn require_param_str_returns_error_when_missing() {
    let params = serde_json::json!({});
    let result = require_param_str(&params, &["sandbox", "sandbox_id"]);
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("missing 'sandbox'"), "got: {}", msg);
}

#[test]
fn parse_string_array_extracts_strings() {
    let params = serde_json::json!({
        "command": ["ls", "-la", "/tmp"],
    });
    let result = parse_string_array(&params, "command").unwrap();
    assert_eq!(result, vec!["ls", "-la", "/tmp"]);
}

#[test]
fn parse_string_array_rejects_missing_key() {
    let params = serde_json::json!({});
    assert!(parse_string_array(&params, "command").is_err());
}

#[test]
fn parse_string_array_rejects_non_string_elements() {
    let params = serde_json::json!({
        "command": ["ls", 42],
    });
    assert!(parse_string_array(&params, "command").is_err());
}
