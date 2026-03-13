use super::*;
use serde_json::json;

#[test]
fn request_deserializes_with_action_and_params() {
    let raw = r#"{"action":"sandbox.create","params":{"name":"test"}}"#;
    let req: Request = serde_json::from_str(raw).unwrap();
    assert_eq!(req.action, "sandbox.create");
    assert_eq!(req.params["name"], "test");
}

#[test]
fn request_deserializes_without_params() {
    let raw = r#"{"action":"ping"}"#;
    let req: Request = serde_json::from_str(raw).unwrap();
    assert_eq!(req.action, "ping");
    assert!(req.params.is_null());
}

#[test]
fn request_rejects_missing_action() {
    let raw = r#"{"params":{}}"#;
    let result: std::result::Result<Request, _> = serde_json::from_str(raw);
    assert!(result.is_err());
}

#[test]
fn response_ok_serializes_correctly() {
    let response = Response::ok(json!({"status": "pong"}));
    let serialized = serde_json::to_value(&response).unwrap();
    assert_eq!(serialized["ok"], true);
    assert_eq!(serialized["result"]["status"], "pong");
    assert!(serialized.get("error").is_none());
}

#[test]
fn response_err_serializes_correctly() {
    let response = Response::err("something went wrong");
    let serialized = serde_json::to_value(&response).unwrap();
    assert_eq!(serialized["ok"], false);
    assert!(serialized.get("result").is_none());
    assert_eq!(serialized["error"], "something went wrong");
}

#[test]
fn response_ok_omits_error_field() {
    let response = Response::ok(json!(null));
    let json_str = serde_json::to_string(&response).unwrap();
    assert!(!json_str.contains("error"));
}

#[test]
fn response_err_omits_result_field() {
    let response = Response::err("fail");
    let json_str = serde_json::to_string(&response).unwrap();
    assert!(!json_str.contains("result"));
}
