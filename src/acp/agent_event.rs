//! Shared event types and helpers for agent stream-json output.
//!
//! Used by both `streaming.rs` (legacy per-prompt process) and
//! `agent_process.rs` (persistent ACP process).

use super::types::{StopReason, ToolKind};

// ---------------------------------------------------------------------------
// Agent stream-json output types (shared between streaming.rs and agent_process.rs)
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// NDJSON I/O
// ---------------------------------------------------------------------------

/// Write a line as NDJSON (with newline) and flush.
pub async fn write_ndjson<W: tokio::io::AsyncWriteExt + Unpin>(
    writer: &mut W,
    line: &str,
) -> std::io::Result<()> {
    writer.write_all(line.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Notification builders
// ---------------------------------------------------------------------------

/// Create an `agent_message_chunk` sessionUpdate notification JSON string.
pub fn make_chunk_notification(session_id: &str, text: &str) -> String {
    use super::types::{AcpResponse, MessageContent, SessionUpdate, SessionUpdateParams};
    let params = SessionUpdateParams {
        sessionId: session_id.to_string(),
        update: SessionUpdate::AgentMessageChunk {
            content: MessageContent::Text { text: text.to_string() },
        },
    };
    serde_json::to_string(&AcpResponse::notification("sessionUpdate", params))
        .unwrap_or_else(|_| "{}".to_string())
}

/// Create a `tool_call` sessionUpdate notification JSON string.
pub fn make_tool_notification(
    session_id: &str,
    tool_call_id: &str,
    title: &str,
    kind: Option<&ToolKind>,
) -> String {
    use super::types::{AcpResponse, SessionUpdate, SessionUpdateParams, ToolCallStatus};
    let params = SessionUpdateParams {
        sessionId: session_id.to_string(),
        update: SessionUpdate::ToolCall {
            toolCallId: tool_call_id.to_string(),
            title: title.to_string(),
            status: ToolCallStatus::InProgress,
            rawInput: None,
            kind: kind.cloned(),
        },
    };
    serde_json::to_string(&AcpResponse::notification("sessionUpdate", params))
        .unwrap_or_else(|_| "{}".to_string())
}

/// Create a `tool_call_update` sessionUpdate notification JSON string.
pub fn make_tool_update_notification(
    session_id: &str,
    tool_call_id: &str,
    status: super::types::ToolCallStatus,
    output: Option<&str>,
) -> String {
    use super::types::{AcpResponse, MessageContent, SessionUpdate, SessionUpdateParams, ToolCallContent};
    let content = output.map(|o| {
        vec![ToolCallContent::Content {
            content: MessageContent::Text { text: o.to_string() },
        }]
    });
    let params = SessionUpdateParams {
        sessionId: session_id.to_string(),
        update: SessionUpdate::ToolCallUpdate {
            toolCallId: tool_call_id.to_string(),
            status: Some(status),
            rawOutput: output.map(|o| serde_json::json!(o)),
            content,
        },
    };
    serde_json::to_string(&AcpResponse::notification("sessionUpdate", params))
        .unwrap_or_else(|_| "{}".to_string())
}

// ---------------------------------------------------------------------------
// Tool helpers
// ---------------------------------------------------------------------------

/// Format a human-readable title for a tool invocation.
pub fn format_tool_title(name: &str, input: &Option<serde_json::Value>) -> String {
    match name {
        "Read" => {
            let path = extract_str(input, "file_path", "unknown");
            format!("Read({})", path)
        }
        "Edit" => {
            let path = extract_str(input, "file_path", "unknown");
            format!("Edit({})", path)
        }
        "Write" => {
            let path = extract_str(input, "file_path", "unknown");
            format!("Write({})", path)
        }
        "Delete" => {
            let path = extract_str(input, "file_path", "unknown");
            format!("Delete({})", path)
        }
        "Bash" => {
            let cmd = extract_str(input, "command", "unknown");
            let truncated = if cmd.chars().count() > 40 {
                format!("{}...", cmd.chars().take(37).collect::<String>())
            } else {
                cmd.to_string()
            };
            format!("Bash({})", truncated)
        }
        "Glob" => {
            let pattern = extract_str(input, "pattern", "*");
            format!("Glob({})", pattern)
        }
        "Grep" => {
            let pattern = extract_str(input, "pattern", "");
            format!("Grep({})", pattern)
        }
        _ => name.to_string(),
    }
}

/// Infer the tool kind from its name.
pub fn infer_tool_kind(name: &str) -> Option<ToolKind> {
    match name {
        "Read" => Some(ToolKind::Read),
        "Edit" => Some(ToolKind::Edit),
        "Write" => Some(ToolKind::Other),
        "Delete" => Some(ToolKind::Delete),
        "Glob" | "Grep" => Some(ToolKind::Search),
        "Bash" => Some(ToolKind::Execute),
        "WebFetch" => Some(ToolKind::Fetch),
        _ => Some(ToolKind::Other),
    }
}

// ---------------------------------------------------------------------------
// Misc helpers
// ---------------------------------------------------------------------------

/// Convert StopReason to string for JSON responses.
pub fn stop_reason_str(reason: &StopReason) -> &'static str {
    match reason {
        StopReason::EndTurn => "end_turn",
        StopReason::Stop => "stop",
        StopReason::Cancelled => "cancelled",
        StopReason::Refusal => "refusal",
        StopReason::Error => "error",
    }
}

/// Truncate text to `max_chars` characters, appending "..." if truncated.
pub fn truncate_preview(s: &str, max_chars: usize) -> String {
    if s.chars().count() > max_chars {
        format!("{}...", s.chars().take(max_chars).collect::<String>())
    } else {
        s.to_string()
    }
}

fn extract_str(input: &Option<serde_json::Value>, key: &str, default: &str) -> String {
    input
        .as_ref()
        .and_then(|i| i.get(key))
        .and_then(|v| v.as_str())
        .unwrap_or(default)
        .to_string()
}
