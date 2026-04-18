//! Simplified protocol type definitions
//!
//! aginx uses a minimal JSON-RPC 2.0 protocol:
//! Client sends prompt → aginx spawns CLI → streams stdout chunks back → sends final result

use serde::{Deserialize, Serialize};

// ============================================================================
// JSON-RPC 2.0 Base Types
// ============================================================================

/// JSON-RPC request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Id>,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

/// JSON-RPC response or notification
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Id>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

/// Request ID (string or number per JSON-RPC 2.0)
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Id {
    String(String),
    Number(i64),
}

/// JSON-RPC error
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

// ============================================================================
// Method-specific Params
// ============================================================================

/// Prompt request params
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptParams {
    /// Agent ID to route to
    pub agent: String,
    /// User message
    pub message: String,
    /// Optional auth token
    #[serde(default)]
    pub token: Option<String>,
    /// Optional session ID for resume (e.g. Claude --resume)
    #[serde(default)]
    pub sessionId: Option<String>,
    /// Optional working directory
    #[serde(default)]
    pub cwd: Option<String>,
}

/// List agents params
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListAgentsParams {}

// ============================================================================
// Response Types
// ============================================================================

/// Prompt final result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptResult {
    pub stopReason: String,
    pub sessionId: String,
}

/// Chunk notification params (streaming text)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkParams {
    pub text: String,
}

/// Agent info for listAgents
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInfo {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

// ============================================================================
// Response Helpers
// ============================================================================

impl Response {
    pub fn success(id: Option<Id>, result: impl Serialize) -> Self {
        let result_value = serde_json::to_value(&result).unwrap_or_else(|e| {
            tracing::error!("Failed to serialize response: {}", e);
            serde_json::json!({"error": format!("Serialization failed: {}", e)})
        });
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            method: None,
            params: None,
            result: Some(result_value),
            error: None,
        }
    }

    pub fn error(id: Option<Id>, code: i32, message: &str) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            method: None,
            params: None,
            result: None,
            error: Some(RpcError {
                code,
                message: message.to_string(),
                data: None,
            }),
        }
    }

    pub fn notification(method: &str, params: impl Serialize) -> Self {
        let params_value = serde_json::to_value(&params).unwrap_or(serde_json::Value::Null);
        Self {
            jsonrpc: "2.0".to_string(),
            id: None,
            method: Some(method.to_string()),
            params: Some(params_value),
            result: None,
            error: None,
        }
    }

    pub fn to_ndjson(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

// Legacy type aliases for compatibility with server/relay code
pub type AcpRequest = Request;
pub type AcpResponse = Response;
pub type AcpError = RpcError;
