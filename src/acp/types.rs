#![allow(non_snake_case)]
//! ACP protocol type definitions
//! Based on ACP 0.15.0 specification

use serde::{Deserialize, Serialize};

/// ACP protocol version
pub const ACP_VERSION: &str = "0.15.0";

/// ACP Agent name
pub const ACP_AGENT_NAME: &str = "aginx";
pub const ACP_AGENT_TITLE: &str = "Aginx ACP Gateway";

// ============================================================================
// Request Types
// ============================================================================

/// ACP request wrapper
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpRequest {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Id>,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

impl AcpRequest {
    /// Parse required params, returns error response on failure
    pub fn parse_params<T: serde::de::DeserializeOwned>(&self) -> Result<T, AcpResponse> {
        match &self.params {
            Some(p) => serde_json::from_value(p.clone())
                .map_err(|e| AcpResponse::error(self.id.clone(), -32602, &format!("Invalid params: {}", e))),
            None => Err(AcpResponse::error(self.id.clone(), -32602, "Missing params")),
        }
    }

    /// Parse optional params with a default fallback
    pub fn parse_params_or_default<T: serde::de::DeserializeOwned + Default>(&self) -> Result<T, AcpResponse> {
        match &self.params {
            Some(p) => serde_json::from_value(p.clone())
                .map_err(|e| AcpResponse::error(self.id.clone(), -32602, &format!("Invalid params: {}", e))),
            None => Ok(T::default()),
        }
    }

    /// Get params as raw JSON Value with a default empty object
    pub fn params_value(&self) -> serde_json::Value {
        self.params.clone().unwrap_or(serde_json::json!({}))
    }
}

/// Request ID type
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Id {
    String(String),
    Number(i64),
}

/// Initialize request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeParams {
    pub protocolVersion: String,
    #[serde(default)]
    pub clientCapabilities: ClientCapabilities,
    #[serde(default)]
    pub clientInfo: ClientInfo,
    #[serde(default)]
    pub _meta: Option<serde_json::Value>,
}

/// Client capabilities
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClientCapabilities {
    #[serde(default)]
    pub fs: Option<FsCapabilities>,
    #[serde(default)]
    pub terminal: Option<bool>,
}

/// Filesystem capabilities
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FsCapabilities {
    #[serde(default)]
    pub readTextFile: Option<bool>,
    #[serde(default)]
    pub writeTextFile: Option<bool>,
}

/// Client info
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClientInfo {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
}

/// New session request
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NewSessionParams {
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub mcpServers: Vec<McpServer>,
    #[serde(default)]
    pub _meta: Option<serde_json::Value>,
}

/// Load session request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadSessionParams {
    pub sessionId: String,
    #[serde(default)]
    pub agentId: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub mcpServers: Vec<McpServer>,
}

/// MCP server configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServer {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub config: serde_json::Value,
}

/// Prompt request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptParams {
    pub sessionId: String,
    pub prompt: Vec<ContentBlock>,
    #[serde(default)]
    pub _meta: Option<serde_json::Value>,
}

/// Content block for prompt
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image { data: String, mimeType: String },
    #[serde(rename = "resource")]
    Resource { resource: ResourceContent },
    #[serde(rename = "resource_link")]
    ResourceLink { uri: String, #[serde(default)] title: Option<String> },
}

/// Resource content
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceContent {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub mimeType: Option<String>,
}

/// Cancel request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelParams {
    pub sessionId: String,
}

// ============================================================================
// Response Types
// ============================================================================

/// ACP response/notification wrapper
///
/// For responses: has `id` + `result` or `error`
/// For notifications: has `method` + `params`, no `id`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpResponse {
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
    pub error: Option<AcpError>,
}

/// ACP error
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// Initialize response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeResult {
    pub protocolVersion: String,
    #[serde(default)]
    pub agentCapabilities: AgentCapabilities,
    #[serde(default)]
    pub agentInfo: AcpAgentInfo,
    #[serde(default)]
    pub authMethods: Vec<AuthMethod>,
    #[serde(default)]
    pub authenticated: bool,
}

/// Agent capabilities
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCapabilities {
    #[serde(default)]
    pub loadSession: Option<bool>,
    #[serde(default)]
    pub promptCapabilities: Option<PromptCapabilities>,
    #[serde(default)]
    pub mcpCapabilities: Option<McpCapabilities>,
    #[serde(default)]
    pub sessionCapabilities: Option<SessionCapabilities>,
}

/// Prompt capabilities
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptCapabilities {
    #[serde(default)]
    pub image: Option<bool>,
    #[serde(default)]
    pub audio: Option<bool>,
    #[serde(default)]
    pub embeddedContext: Option<bool>,
}

/// MCP capabilities
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpCapabilities {
    #[serde(default)]
    pub http: Option<bool>,
    #[serde(default)]
    pub sse: Option<bool>,
}

/// Session capabilities
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionCapabilities {
    #[serde(default)]
    pub list: Option<serde_json::Value>,
}

/// Agent info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpAgentInfo {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
}

impl Default for AcpAgentInfo {
    fn default() -> Self {
        Self {
            name: ACP_AGENT_NAME.to_string(),
            title: Some(ACP_AGENT_TITLE.to_string()),
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
        }
    }
}

/// Auth method
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthMethod {
    #[serde(rename = "type")]
    pub auth_type: String,
    #[serde(default)]
    pub label: Option<String>,
}

/// Stop reason
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StopReason {
    #[serde(rename = "end_turn")]
    EndTurn,
    #[serde(rename = "stop")]
    Stop,
    #[serde(rename = "cancelled")]
    Cancelled,
    #[serde(rename = "refusal")]
    Refusal,
    #[serde(rename = "error")]
    Error,
}

// ============================================================================
// Notification Types (Server -> Client)
// ============================================================================

/// Session update notification
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionUpdateParams {
    pub sessionId: String,
    pub update: SessionUpdate,
}

/// Session update types
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "sessionUpdate")]
pub enum SessionUpdate {
    #[serde(rename = "agent_message_chunk")]
    AgentMessageChunk { content: MessageContent },

    #[serde(rename = "tool_call")]
    ToolCall {
        toolCallId: String,
        title: String,
        status: ToolCallStatus,
        #[serde(default)]
        rawInput: Option<serde_json::Value>,
        #[serde(default)]
        kind: Option<ToolKind>,
    },

    #[serde(rename = "tool_call_update")]
    ToolCallUpdate {
        toolCallId: String,
        #[serde(default)]
        status: Option<ToolCallStatus>,
        #[serde(default)]
        rawOutput: Option<serde_json::Value>,
        #[serde(default)]
        content: Option<Vec<ToolCallContent>>,
    },

    #[serde(rename = "available_commands_update")]
    AvailableCommandsUpdate {
        availableCommands: Vec<CommandInfo>,
    },
}

/// Message content
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum MessageContent {
    #[serde(rename = "text")]
    Text { text: String },
}

/// Tool call status
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolCallStatus {
    InProgress,
    Completed,
    Failed,
}

/// Tool kind
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolKind {
    Read,
    Edit,
    Delete,
    Move,
    Search,
    Execute,
    Fetch,
    Other,
}

/// Tool call content
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ToolCallContent {
    #[serde(rename = "content")]
    Content { content: MessageContent },
    #[serde(rename = "location")]
    Location { path: String, #[serde(skip_serializing_if = "Option::is_none")] line: Option<u32> },
}

/// Command info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandInfo {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

// ============================================================================
// Helper implementations
// ============================================================================

impl AcpResponse {
    /// Create a success response
    pub fn success(id: Option<Id>, result: impl Serialize) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            method: None,
            params: None,
            result: Some(serde_json::to_value(result).unwrap_or(serde_json::Value::Null)),
            error: None,
        }
    }

    /// Create an error response
    pub fn error(id: Option<Id>, code: i32, message: &str) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            method: None,
            params: None,
            result: None,
            error: Some(AcpError {
                code,
                message: message.to_string(),
                data: None,
            }),
        }
    }

    /// Create a notification (no id, has method + params at top level)
    pub fn notification(method: &str, params: impl Serialize) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id: None,
            method: Some(method.to_string()),
            params: Some(serde_json::to_value(params).unwrap_or(serde_json::Value::Null)),
            result: None,
            error: None,
        }
    }

    /// Convert to JSON string (ndjson format)
    pub fn to_ndjson(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

impl Default for PromptCapabilities {
    fn default() -> Self {
        Self {
            image: Some(true),
            audio: Some(false),
            embeddedContext: Some(true),
        }
    }
}

impl InitializeResult {
    pub fn new() -> Self {
        Self {
            protocolVersion: ACP_VERSION.to_string(),
            agentCapabilities: AgentCapabilities::default(),
            agentInfo: AcpAgentInfo {
                name: ACP_AGENT_NAME.to_string(),
                title: Some(ACP_AGENT_TITLE.to_string()),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            },
            authMethods: vec![],
            authenticated: false,
        }
    }
}

impl Default for AgentCapabilities {
    fn default() -> Self {
        Self {
            loadSession: Some(true),
            promptCapabilities: Some(PromptCapabilities::default()),
            mcpCapabilities: Some(McpCapabilities {
                http: Some(false),
                sse: Some(false),
            }),
            sessionCapabilities: Some(SessionCapabilities {
                list: Some(serde_json::Value::Null),
            }),
        }
    }
}
