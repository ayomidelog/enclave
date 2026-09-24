use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Deserialize)]
pub struct Request {
    pub action: String,
    #[serde(default)]
    pub params: Value,
    /// Optional id so a caller can correlate a retry with the attempt it
    /// repeats. Ignored unless it is a plain UUID.
    #[serde(default)]
    pub operation_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Response {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The operation this request ran as. Present on every response so a
    /// failure can be traced to the journal record, the logs, and the metrics
    /// for that one operation.
    pub operation_id: String,
}

impl Response {
    pub fn ok(result: Value, operation_id: impl Into<String>) -> Self {
        Self {
            ok: true,
            result: Some(result),
            error: None,
            operation_id: operation_id.into(),
        }
    }

    pub fn err(message: impl Into<String>, operation_id: impl Into<String>) -> Self {
        Self {
            ok: false,
            result: None,
            error: Some(message.into()),
            operation_id: operation_id.into(),
        }
    }
}

#[cfg(test)]
#[path = "../tests/src/protocol.rs"]
mod tests;
