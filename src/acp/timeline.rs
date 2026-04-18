//! Standard Timeline event model for aginx.
//!
//! Provides a unified, typed event representation for all streaming agent output.
//! Each event carries a sequence number, timestamp, and session ID.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Sequence counter for TimelineEvent ordering.
static TIMELINE_SEQ: AtomicU64 = AtomicU64::new(1);

fn next_seq() -> u64 {
    TIMELINE_SEQ.fetch_add(1, Ordering::Relaxed)
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// A typed timeline event with metadata.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TimelineEvent {
    /// Monotonically increasing sequence number
    pub seq: u64,
    /// Unix timestamp in milliseconds
    pub timestamp: u64,
    /// aginx session ID
    pub session_id: String,
    /// The event payload
    #[serde(flatten)]
    pub kind: TimelineEventKind,
}

impl TimelineEvent {
    /// Create a new timeline event with auto-assigned seq and timestamp.
    pub fn new(session_id: impl Into<String>, kind: TimelineEventKind) -> Self {
        Self {
            seq: next_seq(),
            timestamp: now_millis(),
            session_id: session_id.into(),
            kind,
        }
    }

    /// Serialize to JSON string for wire transmission.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Convert to the JSON-RPC session/update notification wire format.
    pub fn to_wire_json(&self) -> String {
        match &self.kind {
            TimelineEventKind::TextChunk { content } => {
                let text = content.get("text").and_then(|t| t.as_str()).unwrap_or("");
                serde_json::to_string(&serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "session/update",
                    "params": {
                        "sessionId": self.session_id,
                        "update": {
                            "sessionUpdate": "agent_message_chunk",
                            "content": {"type": "text", "text": text}
                        }
                    }
                })).unwrap_or_default()
            }
            TimelineEventKind::ThoughtChunk { content } => {
                let text = content.get("text").and_then(|t| t.as_str()).unwrap_or("");
                serde_json::to_string(&serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "session/update",
                    "params": {
                        "sessionId": self.session_id,
                        "update": {
                            "sessionUpdate": "agent_thought_chunk",
                            "content": {"type": "text", "text": text}
                        }
                    }
                })).unwrap_or_default()
            }
            TimelineEventKind::ToolCallStarted { tool_call_id, title, kind } => {
                serde_json::to_string(&serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "session/update",
                    "params": {
                        "sessionId": self.session_id,
                        "update": {
                            "sessionUpdate": "tool_call",
                            "toolCallId": tool_call_id,
                            "title": title,
                            "status": "in_progress",
                            "kind": kind,
                        }
                    }
                })).unwrap_or_default()
            }
            TimelineEventKind::ToolCallUpdated { tool_call_id, status, title } => {
                serde_json::to_string(&serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "session/update",
                    "params": {
                        "sessionId": self.session_id,
                        "update": {
                            "sessionUpdate": "tool_call_update",
                            "toolCallId": tool_call_id,
                            "status": status,
                            "title": title,
                        }
                    }
                })).unwrap_or_default()
            }
            TimelineEventKind::ToolCallCompleted { tool_call_id, status } => {
                serde_json::to_string(&serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "session/update",
                    "params": {
                        "sessionId": self.session_id,
                        "update": {
                            "sessionUpdate": "tool_call_update",
                            "toolCallId": tool_call_id,
                            "status": status,
                        }
                    }
                })).unwrap_or_default()
            }
            // For all other events, serialize the full TimelineEvent
            _ => self.to_json(),
        }
    }

    /// Parse a raw agent notification JSON into a TimelineEvent.
    /// Returns None if the message is not a recognizable session/update notification.
    pub fn from_agent_notification(raw: &serde_json::Value, session_id: &str) -> Option<Self> {
        let method = raw.get("method").and_then(|m| m.as_str())?;

        // Handle session/request_permission separately
        if method == "session/request_permission" {
            let params = raw.get("params")?;
            let request_id = params.get("requestId").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let description = params.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let options = params.get("options")
                .and_then(|o| o.as_array())
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(|v| v);
            return Some(Self::new(session_id, TimelineEventKind::PermissionRequested {
                request_id,
                description,
                options: params.get("options").and_then(|o| o.as_array()).cloned().unwrap_or_default(),
            }));
        }

        if method != "session/update" {
            return None;
        }

        let update = raw.get("params")?.get("update")?;
        let update_type = update.get("sessionUpdate").and_then(|u| u.as_str())?;

        let kind = match update_type {
            "agent_message_chunk" | "user_message_chunk" => {
                let content = update.get("content").cloned().unwrap_or(serde_json::json!({}));
                TimelineEventKind::TextChunk { content }
            }
            "agent_thought_chunk" => {
                let content = update.get("content").cloned().unwrap_or(serde_json::json!({}));
                TimelineEventKind::ThoughtChunk { content }
            }
            "tool_call" => {
                let tool_call_id = update.get("toolCallId").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let title = update.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let kind = update.get("kind").and_then(|v| v.as_str()).map(String::from);
                TimelineEventKind::ToolCallStarted { tool_call_id, title, kind }
            }
            "tool_call_update" => {
                let tool_call_id = update.get("toolCallId").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let status = update.get("status").and_then(|v| v.as_str()).unwrap_or("completed").to_string();
                let title = update.get("title").and_then(|v| v.as_str()).map(String::from);
                match status.as_str() {
                    "completed" | "failed" | "canceled" => {
                        TimelineEventKind::ToolCallCompleted { tool_call_id, status }
                    }
                    _ => {
                        TimelineEventKind::ToolCallUpdated { tool_call_id, status, title }
                    }
                }
            }
            "usage_update" => {
                let usage = update.get("usage").cloned().unwrap_or(serde_json::json!({}));
                TimelineEventKind::UsageUpdated { usage }
            }
            _ => return None,
        };

        Some(Self::new(session_id, kind))
    }
}

/// Typed event kinds for agent streaming output.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type")]
pub enum TimelineEventKind {
    /// A new prompt turn has started
    TurnStarted,
    /// The current turn completed successfully
    TurnCompleted { stop_reason: String },
    /// The current turn failed
    TurnFailed { error: String },
    /// Streaming text chunk from the agent
    TextChunk { content: serde_json::Value },
    /// Agent thinking/reasoning chunk
    ThoughtChunk { content: serde_json::Value },
    /// A tool invocation has started
    ToolCallStarted {
        tool_call_id: String,
        title: String,
        kind: Option<String>,
    },
    /// A tool invocation status updated (in_progress, pending)
    ToolCallUpdated {
        tool_call_id: String,
        status: String,
        title: Option<String>,
    },
    /// A tool invocation finished (completed, failed, canceled)
    ToolCallCompleted {
        tool_call_id: String,
        status: String,
    },
    /// Agent requests user permission
    PermissionRequested {
        request_id: String,
        description: String,
        options: Vec<serde_json::Value>,
    },
    /// Permission request resolved
    PermissionResolved {
        request_id: String,
        outcome: String,
    },
    /// Token usage update
    UsageUpdated { usage: serde_json::Value },
}

/// Typed event sender — wraps mpsc::Sender<String>, serializes TimelineEvents to wire JSON.
#[derive(Clone)]
pub struct EventSender {
    inner: tokio::sync::mpsc::Sender<String>,
}

impl EventSender {
    /// Create a new EventSender wrapping an mpsc channel.
    pub fn new(inner: tokio::sync::mpsc::Sender<String>) -> Self {
        Self { inner }
    }

    /// Send a typed TimelineEvent, serialized to wire JSON format.
    pub async fn send_event(&self, event: TimelineEvent) -> Result<(), tokio::sync::mpsc::error::SendError<String>> {
        let json = event.to_wire_json();
        self.inner.send(json).await
    }

    /// Send an AcpResponse as JSON (for final prompt responses).
    pub async fn send_response(&self, resp: &crate::acp::types::AcpResponse) -> Result<(), tokio::sync::mpsc::error::SendError<String>> {
        let json = serde_json::to_string(resp).unwrap_or_default();
        self.inner.send(json).await
    }
}
