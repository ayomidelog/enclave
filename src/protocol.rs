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
    /// The kind of failure, so a caller can react to it without reading the
    /// message. Absent on success.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<crate::error::ErrorCode>,
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
            code: None,
            operation_id: operation_id.into(),
        }
    }

    /// A failure that names its category.
    ///
    /// Every failure carries a code. A failure nobody categorized is
    /// [`crate::error::ErrorCode::Internal`], which the daemon's dispatch
    /// boundary supplies through [`crate::error::code_of`].
    pub fn err_code(
        code: crate::error::ErrorCode,
        message: impl Into<String>,
        operation_id: impl Into<String>,
    ) -> Self {
        Self {
            ok: false,
            result: None,
            error: Some(message.into()),
            code: Some(code),
            operation_id: operation_id.into(),
        }
    }
}

#[cfg(test)]
#[path = "../tests/src/protocol.rs"]
mod tests;
