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
        "sandbox.pause",
        "sandbox.resume",
        "sandbox.status",
        "sandbox.destroy",
        "sandbox.list",
        "sandbox.remove",
        "sandbox.exec_setup",
        "process.list",
        "workspace.create",
        "workspace.start",
        "workspace.start_many",
        "workspace.stop",
        "workspace.destroy",
        "workspace.wipe",
        "workspace.status",
        "workspace.stats",
        "workspace.stats.list",
        "workspace.list",
        "workspace.remove",
        "workspace.update",
        "workspace.update_auth",
        "workspace.resize",
        "workspace.exec",
        "workspace.cp",
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

#[test]
fn parse_required_disk_bytes_uses_checked_mib_conversion() {
    let params = serde_json::json!({"disk_mb": 64});
    assert_eq!(
        parse_required_disk_bytes(&params).unwrap(),
        64 * 1024 * 1024
    );
}

#[test]
fn parse_required_disk_bytes_rejects_missing_and_overflowing_values() {
    assert!(parse_required_disk_bytes(&serde_json::json!({})).is_err());
    assert!(parse_required_disk_bytes(&serde_json::json!({
        "disk_mb": u64::MAX
    }))
    .is_err());
}

#[test]
fn workspace_limit_parsing_rejects_overflow_in_create_and_update() {
    let params = serde_json::json!({"memory_mb": u64::MAX, "disk_mb": u64::MAX});
    assert!(parse_workspace_limits_create(&params)
        .expect_err("create overflow must fail")
        .to_string()
        .contains("memory_mb"));
    assert!(parse_workspace_limits_update(&params)
        .expect_err("update overflow must fail")
        .to_string()
        .contains("memory_mb"));
}

#[test]
fn clear_tmp_on_restart_accepts_boolean_values() {
    assert_eq!(
        parse_optional_bool_field(
            &serde_json::json!({"clear_tmp_on_restart": true}),
            "clear_tmp_on_restart"
        )
        .unwrap(),
        Some(true)
    );
}

#[test]
fn clear_tmp_on_restart_rejects_non_boolean_values() {
    assert!(parse_optional_bool_field(
        &serde_json::json!({"clear_tmp_on_restart": "yes"}),
        "clear_tmp_on_restart"
    )
    .is_err());
}

/// `workspace.start_many` reuses a start item as an update when the workspace
/// already exists. `disk_mb` is valid on a start item but cannot be applied by
/// an update, so forwarding it made every `up` after a `down` fail for
/// quota-backed workspaces.
#[test]
fn existing_workspace_update_drops_the_declared_disk_allocation() {
    let spec = serde_json::json!({
        "sandbox_id": "sandbox-id",
        "name": "dev",
        "disk_mb": 64,
        "memory_mb": 256,
        "clear_tmp_on_restart": true,
        "path": serde_json::Value::Null,
    });

    let update = existing_workspace_update(&spec, "sandbox-id", "dev");
    let object = update.as_object().expect("update is an object");

    assert!(!object.contains_key("disk_mb"));
    assert!(!object.contains_key("path"));
    assert_eq!(
        object.get("sandbox").and_then(Value::as_str),
        Some("sandbox-id")
    );
    assert_eq!(object.get("workspace").and_then(Value::as_str), Some("dev"));
    assert_eq!(object.get("memory_mb").and_then(Value::as_u64), Some(256));
    assert_eq!(
        object.get("clear_tmp_on_restart").and_then(Value::as_bool),
        Some(true)
    );
}

/// The declared disk allocation is the only field with update-incompatible
/// semantics, so the rest of the definition must still reach the update.
#[test]
fn existing_workspace_update_keeps_supported_limits() {
    let spec = serde_json::json!({
        "name": "dev",
        "cpu_percent": 25.0,
        "cpu_seconds": 30,
        "max_procs": 64,
        "max_open_files": 1024,
    });

    let update = existing_workspace_update(&spec, "sb", "dev");
    let parsed = parse_workspace_limits_update(&update).expect("update limits parse");
    assert_eq!(parsed.cpu_seconds, Some(Some(30)));
    assert_eq!(parsed.cpu_percent, Some(Some(25.0)));
    assert_eq!(parsed.max_processes, Some(Some(64)));
    assert_eq!(parsed.max_open_files, Some(Some(1024)));
    assert_eq!(parsed.disk_bytes, None);
}
