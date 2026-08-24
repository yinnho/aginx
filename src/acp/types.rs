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
/// (sessionId/sessionTicket/activeFlow 保持 wire 原 camelCase 形状，ACP.md §2)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(non_snake_case)]
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
    /// 借用轮：会话票据（进/出，主人服务器无状态）。带此字段走 ACP 直通。
    #[serde(default)]
    pub sessionTicket: Option<serde_json::Value>,
    /// 借用轮素材：[{name, contentBase64}]，仅本轮有效
    #[serde(default)]
    pub materials: Option<serde_json::Value>,
    /// 借用轮显式 flow（按名加载，不做 LLM classify）
    #[serde(default)]
    pub activeFlow: Option<String>,
    /// 借用者身份。优先级：鉴权身份 > 客户端显式传入——显式值只在无鉴权
    /// （public 模式）时被采纳，防冒名。
    #[serde(default)]
    pub borrower: Option<String>,
}

// ============================================================================
// Response Types
// ============================================================================

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

    pub fn to_ndjson(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

// Legacy type aliases for compatibility with server/relay code
pub type AcpRequest = Request;
pub type AcpResponse = Response;
