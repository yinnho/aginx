//! JSON-RPC 2.0 types for aginx
//!
//! Implements the JSON-RPC 2.0 specification for request/response messaging.

use serde::{Deserialize, Serialize};
use std::fmt;

/// JSON-RPC version string
pub const JSONRPC_VERSION: &str = "2.0";

/// Request ID type - can be String, Number, or null
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    /// String ID
    String(String),
    /// Numeric ID
    Number(i64),
    /// Null ID (for notifications without response)
    Null,
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RequestId::String(s) => write!(f, "{}", s),
            RequestId::Number(n) => write!(f, "{}", n),
            RequestId::Null => write!(f, "null"),
        }
    }
}

impl From<&str> for RequestId {
    fn from(s: &str) -> Self {
        RequestId::String(s.to_string())
    }
}

impl From<String> for RequestId {
    fn from(s: String) -> Self {
        RequestId::String(s)
    }
}

impl From<i64> for RequestId {
    fn from(n: i64) -> Self {
        RequestId::Number(n)
    }
}

impl From<u64> for RequestId {
    fn from(n: u64) -> Self {
        RequestId::Number(n as i64)
    }
}

impl From<usize> for RequestId {
    fn from(n: usize) -> Self {
        RequestId::Number(n as i64)
    }
}

impl From<i32> for RequestId {
    fn from(n: i32) -> Self {
        RequestId::Number(n as i64)
    }
}

/// JSON-RPC 2.0 Request
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    /// JSON-RPC version (must be "2.0")
    pub jsonrpc: String,
    /// Request ID (null for notifications)
    pub id: Option<RequestId>,
    /// Method name
    pub method: String,
    /// Method parameters
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

impl JsonRpcRequest {
    /// Create a new request with ID
    pub fn new<I: Into<RequestId>>(id: I, method: &str, params: Option<serde_json::Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: Some(id.into()),
            method: method.to_string(),
            params,
        }
    }

    /// Create a notification (no ID, no response expected)
    pub fn notification(method: &str, params: Option<serde_json::Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: None,
            method: method.to_string(),
            params,
        }
    }

    /// Check if this is a notification (no response expected)
    pub fn is_notification(&self) -> bool {
        self.id.is_none()
    }

    /// Parse from JSON string
    pub fn from_json(json: &str) -> Result<Self, JsonRpcError> {
        serde_json::from_str(json).map_err(|e| JsonRpcError::parse_error(e.to_string()))
    }

    /// Serialize to JSON string
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// JSON-RPC 2.0 Response
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    /// JSON-RPC version (must be "2.0")
    pub jsonrpc: String,
    /// Request ID (matches the request)
    pub id: Option<RequestId>,
    /// Result (present on success)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    /// Error (present on failure)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcErrorObject>,
}

impl JsonRpcResponse {
    /// Create a success response
    pub fn success<I: Into<RequestId>>(id: Option<I>, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: id.map(|i| i.into()),
            result: Some(result),
            error: None,
        }
    }

    /// Create an error response
    pub fn error<I: Into<RequestId>>(id: Option<I>, error: JsonRpcErrorObject) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            id: id.map(|i| i.into()),
            result: None,
            error: Some(error),
        }
    }

    /// Check if this is a success response
    pub fn is_success(&self) -> bool {
        self.error.is_none()
    }

    /// Check if this is an error response
    pub fn is_error(&self) -> bool {
        self.error.is_some()
    }

    /// Parse from JSON string
    pub fn from_json(json: &str) -> Result<Self, JsonRpcError> {
        serde_json::from_str(json).map_err(|e| JsonRpcError::parse_error(e.to_string()))
    }

    /// Serialize to JSON string
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// JSON-RPC Error Object
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonRpcErrorObject {
    /// Error code
    pub code: i32,
    /// Error message
    pub message: String,
    /// Additional error data
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl JsonRpcErrorObject {
    /// Create a new error object
    pub fn new(code: i32, message: &str, data: Option<serde_json::Value>) -> Self {
        Self {
            code,
            message: message.to_string(),
            data,
        }
    }

    /// Parse error (-32700): Invalid JSON
    pub fn parse_error(message: impl Into<String>) -> Self {
        Self::new(-32700, &message.into(), None)
    }

    /// Invalid request (-32600): Invalid JSON-RPC request
    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(-32600, &message.into(), None)
    }

    /// Method not found (-32601): Method does not exist
    pub fn method_not_found(method: &str) -> Self {
        Self::new(-32601, &format!("Method not found: {}", method), None)
    }

    /// Invalid params (-32602): Invalid method parameters
    pub fn invalid_params(message: &str) -> Self {
        Self::new(-32602, message, None)
    }

    /// Internal error (-32603): Internal server error
    pub fn internal_error(message: &str) -> Self {
        Self::new(-32603, message, None)
    }
}

/// JSON-RPC Error (wrapper for error handling)
#[derive(Debug, Clone, thiserror::Error)]
pub enum JsonRpcError {
    /// Parse error
    #[error("Parse error: {0}")]
    ParseError(String),

    /// Invalid request
    #[error("Invalid request: {0}")]
    InvalidRequest(String),

    /// Method not found
    #[error("Method not found: {0}")]
    MethodNotFound(String),

    /// Invalid params
    #[error("Invalid params: {0}")]
    InvalidParams(String),

    /// Internal error
    #[error("Internal error: {0}")]
    InternalError(String),

    /// Application error (custom code)
    #[error("Error {code}: {message}")]
    ApplicationError {
        code: i32,
        message: String,
    },
}

impl JsonRpcError {
    /// Create a parse error
    pub fn parse_error(message: String) -> Self {
        Self::ParseError(message)
    }

    /// Convert to error object
    pub fn to_error_object(&self) -> JsonRpcErrorObject {
        match self {
            Self::ParseError(msg) => JsonRpcErrorObject::parse_error(msg.clone()),
            Self::InvalidRequest(msg) => JsonRpcErrorObject::invalid_request(msg.clone()),
            Self::MethodNotFound(method) => JsonRpcErrorObject::method_not_found(method),
            Self::InvalidParams(msg) => JsonRpcErrorObject::invalid_params(msg),
            Self::InternalError(msg) => JsonRpcErrorObject::internal_error(msg),
            Self::ApplicationError { code, message } => {
                JsonRpcErrorObject::new(*code, message, None)
            }
        }
    }

    /// Create a response from this error
    pub fn to_response(&self, id: Option<RequestId>) -> JsonRpcResponse {
        JsonRpcResponse::error(id, self.to_error_object())
    }
}

/// Helper to build JSON-RPC requests
pub struct JsonRpcBuilder {
    next_id: u64,
}

impl JsonRpcBuilder {
    /// Create a new builder
    pub fn new() -> Self {
        Self { next_id: 1 }
    }

    /// Build a request with auto-incrementing ID
    pub fn request(&mut self, method: &str, params: Option<serde_json::Value>) -> JsonRpcRequest {
        let id = self.next_id;
        self.next_id += 1;
        JsonRpcRequest::new(id, method, params)
    }

    /// Build a notification
    pub fn notification(method: &str, params: Option<serde_json::Value>) -> JsonRpcRequest {
        JsonRpcRequest::notification(method, params)
    }
}

impl Default for JsonRpcBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_request_serialization() {
        let req = JsonRpcRequest::new(1, "sendMessage", Some(json!({"agent": "hotel", "message": "hello"})));
        let json = req.to_json();
        assert!(json.contains(r#""jsonrpc":"2.0""#));
        assert!(json.contains(r#""id":1"#));
        assert!(json.contains(r#""method":"sendMessage""#));
    }

    #[test]
    fn test_request_deserialization() {
        let json = r#"{"jsonrpc":"2.0","id":1,"method":"sendMessage","params":{"agent":"hotel"}}"#;
        let req: JsonRpcRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.jsonrpc, "2.0");
        assert_eq!(req.id, Some(RequestId::Number(1)));
        assert_eq!(req.method, "sendMessage");
    }

    #[test]
    fn test_response_success() {
        let resp = JsonRpcResponse::success(Some(1), json!({"response": "ok"}));
        assert!(resp.is_success());
        assert!(!resp.is_error());
    }

    #[test]
    fn test_response_error() {
        let resp = JsonRpcResponse::error(Some(1), JsonRpcErrorObject::method_not_found("test"));
        assert!(!resp.is_success());
        assert!(resp.is_error());
    }

    #[test]
    fn test_notification() {
        let notif = JsonRpcRequest::notification("ping", None);
        assert!(notif.is_notification());
    }
}
