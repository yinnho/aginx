//! Claude CLI stream-json 输出事件类型
//!
//! Claude CLI 使用 `--output-format stream-json` 时的输出格式。
//! 这些类型仅用于 Claude adapter，不是通用 ACP 协议的一部分。

/// Agent stream-json output event
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(tag = "type")]
#[allow(dead_code)]
pub(crate) enum AgentEvent {
    #[serde(rename = "system")]
    System {
        #[serde(default)]
        session_id: Option<String>,
        #[serde(default)]
        subtype: Option<String>,
    },
    #[serde(rename = "assistant")]
    Assistant {
        message: Option<AssistantMessage>,
        #[serde(default)]
        session_id: Option<String>,
    },
    #[serde(rename = "stream_event")]
    StreamEvent {
        #[serde(rename = "event")]
        inner: StreamInnerEvent,
    },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        #[serde(rename = "tool_use_id")]
        tool_use_id: String,
        #[serde(default)]
        content: Option<serde_json::Value>,
        #[serde(rename = "is_error")]
        #[serde(default)]
        is_error: Option<bool>,
    },
    #[serde(rename = "result")]
    Result {
        #[serde(default)]
        result: Option<String>,
        #[serde(default)]
        stop_reason: Option<String>,
        #[serde(default)]
        session_id: Option<String>,
        #[serde(default)]
        is_error: Option<bool>,
        #[serde(default)]
        subtype: Option<String>,
    },
    #[serde(rename = "error")]
    Error {
        #[serde(default)]
        error: Option<String>,
        #[serde(default)]
        session_id: Option<String>,
    },
}

/// Assistant message with content blocks
#[derive(Debug, Clone, serde::Deserialize)]
pub(crate) struct AssistantMessage {
    #[serde(default)]
    pub content: Vec<ContentBlock>,
}

/// Content block within an assistant message
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(tag = "type")]
pub(crate) enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "thinking")]
    Thinking { thinking: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResultBlock {
        #[serde(rename = "tool_use_id")]
        tool_use_id: String,
        #[serde(default)]
        content: Option<serde_json::Value>,
    },
}

/// Inner event from stream_event
#[derive(Debug, Clone, serde::Deserialize)]
pub(crate) struct StreamInnerEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    pub delta: Option<StreamDelta>,
}

/// Delta from stream_event
#[derive(Debug, Clone, serde::Deserialize)]
pub(crate) struct StreamDelta {
    #[serde(rename = "type")]
    pub delta_type: String,
    #[serde(default)]
    pub text: String,
    #[serde(rename = "stop_reason")]
    pub stop_reason: Option<String>,
}
