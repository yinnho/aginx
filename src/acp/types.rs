#![allow(non_snake_case)]
//! ACP protocol type definitions
//! Based on Agent Client Protocol (agentclientprotocol.com) + Aginx extensions
//! Aligned with Zed ACP standard, protocol version 1

use serde::{Deserialize, Serialize};

/// ACP protocol version
pub const ACP_VERSION: &str = "0.1.0";

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

/// Request ID type (string, number, or null per JSON-RPC 2.0)
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Id {
    String(String),
    Number(i64),
}

/// Initialize request (standard ACP)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeParams {
    pub protocolVersion: String,
    #[serde(default)]
    pub clientCapabilities: ClientCapabilities,
    #[serde(default)]
    pub clientInfo: Option<Implementation>,
    #[serde(default)]
    pub _meta: Option<serde_json::Value>,
}

/// Implementation info (client or agent)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Implementation {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub version: String,
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

/// Authenticate request (standard ACP)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthenticateParams {
    pub methodId: String,
    #[serde(default)]
    pub _meta: Option<serde_json::Value>,
}

/// New session request (standard ACP: cwd and mcpServers are required)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NewSessionParams {
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub mcpServers: Vec<McpServer>,
    #[serde(default)]
    pub _meta: Option<serde_json::Value>,
}

/// Load session request (standard ACP)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadSessionParams {
    pub sessionId: String,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub mcpServers: Vec<McpServer>,
    #[serde(default)]
    pub _meta: Option<serde_json::Value>,
}

/// MCP server configuration (stdio transport, default)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServer {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Option<Vec<String>>,
    #[serde(default)]
    pub env: Option<Vec<EnvVariable>>,
    #[serde(default)]
    pub config: Option<serde_json::Value>,
}

/// Environment variable
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvVariable {
    pub name: String,
    pub value: String,
}

/// Prompt request (standard ACP)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptParams {
    pub sessionId: String,
    pub prompt: Vec<ContentBlock>,
    #[serde(default)]
    pub _meta: Option<serde_json::Value>,
}

/// Content block for prompt (standard ACP, compatible with MCP)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        annotations: Option<Annotations>,
    },
    #[serde(rename = "image")]
    Image {
        data: String,
        mimeType: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        uri: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        annotations: Option<Annotations>,
    },
    #[serde(rename = "audio")]
    Audio {
        data: String,
        mimeType: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        annotations: Option<Annotations>,
    },
    #[serde(rename = "resource")]
    Resource {
        resource: ResourceContent,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        annotations: Option<Annotations>,
    },
    #[serde(rename = "resource_link")]
    ResourceLink {
        uri: String,
        #[serde(default)]
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mimeType: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        annotations: Option<Annotations>,
    },
}

/// Annotations (from MCP)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Annotations {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lastModified: Option<String>,
}

/// Resource content (embedded)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceContent {
    #[serde(default)]
    pub uri: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub mimeType: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob: Option<String>,
}

/// Cancel notification params (standard ACP: notification, no response)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelParams {
    pub sessionId: String,
}

/// Set session mode params (standard ACP)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetModeParams {
    pub sessionId: String,
    pub modeId: String,
}

/// List sessions params (standard ACP)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListSessionsParams {
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub cursor: Option<String>,
}

// ============================================================================
// Response Types
// ============================================================================

/// ACP response/notification wrapper
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

/// Initialize response (standard ACP)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeResult {
    pub protocolVersion: String,
    #[serde(default)]
    pub agentCapabilities: AgentCapabilities,
    #[serde(default)]
    pub agentInfo: Option<Implementation>,
    #[serde(default)]
    pub authMethods: Vec<AuthMethod>,
}

/// Agent capabilities (standard ACP)
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

/// Auth method (standard ACP)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthMethod {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// New session response (standard ACP)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewSessionResult {
    pub sessionId: String,
    #[serde(default)]
    pub modes: Option<SessionModeState>,
    #[serde(default)]
    pub configOptions: Option<Vec<serde_json::Value>>,
}

/// Session mode state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionModeState {
    pub currentModeId: String,
    pub availableModes: Vec<SessionMode>,
}

/// Session mode
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMode {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// Prompt response (standard ACP)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptResult {
    pub stopReason: StopReason,
}

/// Stop reason (standard ACP)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    #[serde(rename = "end_turn")]
    EndTurn,
    #[serde(rename = "max_tokens")]
    MaxTokens,
    #[serde(rename = "max_turn_requests")]
    MaxTurnRequests,
    #[serde(rename = "refusal")]
    Refusal,
    #[serde(rename = "cancelled")]
    Cancelled,
}

// ============================================================================
// Notification Types (Agent -> Client, session/update)
// ============================================================================

/// Session update notification params (standard ACP: method = "session/update")
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionUpdateParams {
    pub sessionId: String,
    pub update: SessionUpdate,
}

/// Session update types (standard ACP)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "sessionUpdate")]
pub enum SessionUpdate {
    #[serde(rename = "agent_message_chunk")]
    AgentMessageChunk { content: ContentBlock },

    #[serde(rename = "user_message_chunk")]
    UserMessageChunk { content: ContentBlock },

    #[serde(rename = "agent_thought_chunk")]
    AgentThoughtChunk { content: ContentBlock },

    #[serde(rename = "tool_call")]
    ToolCall {
        toolCallId: String,
        title: String,
        #[serde(default)]
        kind: Option<ToolKind>,
        #[serde(default)]
        status: Option<ToolCallStatus>,
        #[serde(default)]
        content: Option<Vec<ToolCallContent>>,
        #[serde(default)]
        locations: Option<Vec<ToolCallLocation>>,
        #[serde(default)]
        rawInput: Option<serde_json::Value>,
        #[serde(default)]
        rawOutput: Option<serde_json::Value>,
    },

    #[serde(rename = "tool_call_update")]
    ToolCallUpdate {
        toolCallId: String,
        #[serde(default)]
        status: Option<ToolCallStatus>,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        kind: Option<ToolKind>,
        #[serde(default)]
        content: Option<Vec<ToolCallContent>>,
        #[serde(default)]
        locations: Option<Vec<ToolCallLocation>>,
        #[serde(default)]
        rawInput: Option<serde_json::Value>,
        #[serde(default)]
        rawOutput: Option<serde_json::Value>,
    },

    #[serde(rename = "plan")]
    Plan {
        entries: Vec<PlanEntry>,
    },

    #[serde(rename = "available_commands_update")]
    AvailableCommandsUpdate {
        availableCommands: Vec<CommandInfo>,
    },

    #[serde(rename = "current_mode_update")]
    CurrentModeUpdate {
        currentModeId: String,
    },

    #[serde(rename = "config_option_update")]
    ConfigOptionUpdate {
        configOptions: Vec<serde_json::Value>,
    },

    #[serde(rename = "session_info_update")]
    SessionInfoUpdate {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        updatedAt: Option<String>,
    },
}

/// Tool call status (standard ACP)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStatus {
    Pending,
    #[serde(rename = "in_progress")]
    InProgress,
    Completed,
    Failed,
}

impl Default for ToolCallStatus {
    fn default() -> Self {
        Self::Pending
    }
}

/// Tool kind (standard ACP)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    Read,
    Edit,
    Delete,
    Move,
    Search,
    Execute,
    Think,
    Fetch,
    SwitchMode,
    Other,
}

/// Tool call content (standard ACP)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ToolCallContent {
    #[serde(rename = "content")]
    Content { content: ContentBlock },
    #[serde(rename = "diff")]
    Diff {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        oldText: Option<String>,
        newText: String,
    },
    #[serde(rename = "terminal")]
    Terminal { terminalId: String },
}

/// Tool call location (standard ACP)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallLocation {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

/// Plan entry (standard ACP)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanEntry {
    pub content: String,
    pub priority: PlanEntryPriority,
    pub status: PlanEntryStatus,
}

/// Plan entry priority
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanEntryPriority {
    High,
    Medium,
    Low,
}

/// Plan entry status
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanEntryStatus {
    Pending,
    InProgress,
    Completed,
}

/// Command info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandInfo {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

// ============================================================================
// Permission request types (standard ACP: session/request_permission)
// ============================================================================

/// Permission request (Agent -> Client)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestPermissionParams {
    pub sessionId: String,
    pub toolCall: ToolCallUpdateFields,
    pub options: Vec<PermissionOption>,
}

/// Tool call update fields (subset for permission request)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallUpdateFields {
    pub toolCallId: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub kind: Option<ToolKind>,
    #[serde(default)]
    pub status: Option<ToolCallStatus>,
    #[serde(default)]
    pub content: Option<Vec<ToolCallContent>>,
}

/// Permission option (standard ACP)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionOption {
    pub optionId: String,
    pub name: String,
    pub kind: PermissionOptionKind,
}

/// Permission option kind (standard ACP)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionOptionKind {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
}

/// Permission response outcome (standard ACP)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "outcome")]
pub enum PermissionOutcome {
    #[serde(rename = "cancelled")]
    Cancelled,
    #[serde(rename = "selected")]
    Selected { optionId: String },
}

// ============================================================================
// Helper implementations
// ============================================================================

impl AcpResponse {
    /// Create a success response
    pub fn success(id: Option<Id>, result: impl Serialize) -> Self {
        let result_value = match serde_json::to_value(&result) {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("Failed to serialize success response: {}", e);
                serde_json::json!({"error": format!("Serialization failed: {}", e)})
            }
        };
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            method: None,
            params: None,
            result: Some(result_value),
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

    /// Create a notification (no id, has method + params)
    pub fn notification(method: &str, params: impl Serialize) -> Self {
        let params_value = match serde_json::to_value(&params) {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("Failed to serialize notification params: {}", e);
                serde_json::Value::Null
            }
        };
        Self {
            jsonrpc: "2.0".to_string(),
            id: None,
            method: Some(method.to_string()),
            params: Some(params_value),
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
            agentInfo: Some(Implementation {
                name: ACP_AGENT_NAME.to_string(),
                title: Some(ACP_AGENT_TITLE.to_string()),
                version: env!("CARGO_PKG_VERSION").to_string(),
            }),
            authMethods: vec![],
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
