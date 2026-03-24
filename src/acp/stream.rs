//! Claude CLI stream-json parser
//!
//! Parses the ndjson output from `claude --output-format stream-json --verbose`

use serde::{Deserialize, Serialize};

/// Stream event types from Claude CLI
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum StreamEvent {
    #[serde(rename = "system")]
    System { subtype: String, #[serde(default)] session_id: Option<String> },

    #[serde(rename = "assistant")]
    Assistant { message: AssistantMessage, #[serde(default)] session_id: Option<String> },

    #[serde(rename = "result")]
    Result { subtype: String, result: Option<String>, stop_reason: Option<String>, #[serde(default)] session_id: Option<String> },

    #[serde(rename = "user")]
    User { message: Option<serde_json::Value> },
}

/// Assistant message structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantMessage {
    pub id: Option<String>,
    #[serde(rename = "type")]
    pub msg_type: Option<String>,
    pub role: Option<String>,
    pub content: Vec<ContentBlock>,
    pub stop_reason: Option<String>,
}

/// Content block in a message
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },

    #[serde(rename = "thinking")]
    Thinking { thinking: String, #[serde(default)] signature: String },

    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },

    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: Option<String>,
        is_error: Option<bool>,
    },
}

impl StreamEvent {
    /// Parse a line of stream-json output
    pub fn from_line(line: &str) -> Option<Self> {
        if line.trim().is_empty() {
            return None;
        }
        serde_json::from_str(line).ok()
    }

    /// Check if this is the final result event
    pub fn is_result(&self) -> bool {
        matches!(self, StreamEvent::Result { .. })
    }

    /// Get the stop reason from result event
    pub fn stop_reason(&self) -> Option<&str> {
        match self {
            StreamEvent::Result { stop_reason, .. } => stop_reason.as_deref(),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_system_event() {
        let json = r#"{"type":"system","subtype":"init","session_id":"abc123","tools":[],"mcp_servers":[]}"#;
        let event = StreamEvent::from_line(json).unwrap();
        assert!(matches!(event, StreamEvent::System { .. }));
    }

    #[test]
    fn test_parse_assistant_text() {
        let json = r#"{"type":"assistant","message":{"id":"msg1","type":"message","role":"assistant","content":[{"type":"text","text":"Hello!"}]},"session_id":"abc"}"#;
        let event = StreamEvent::from_line(json).unwrap();
        if let StreamEvent::Assistant { message, .. } = event {
            assert_eq!(message.content.len(), 1);
            if let ContentBlock::Text { text } = &message.content[0] {
                assert_eq!(text, "Hello!");
            } else {
                panic!("Expected text block");
            }
        } else {
            panic!("Expected assistant event");
        }
    }

    #[test]
    fn test_parse_tool_use() {
        let json = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"tool1","name":"Read","input":{"file_path":"/test.rs"}}]},"session_id":"abc"}"#;
        let event = StreamEvent::from_line(json).unwrap();
        if let StreamEvent::Assistant { message, .. } = event {
            if let ContentBlock::ToolUse { name, .. } = &message.content[0] {
                assert_eq!(name, "Read");
            } else {
                panic!("Expected tool_use block");
            }
        }
    }

    #[test]
    fn test_parse_result() {
        let json = r#"{"type":"result","subtype":"success","result":"Done!","stop_reason":"end_turn","session_id":"abc"}"#;
        let event = StreamEvent::from_line(json).unwrap();
        assert_eq!(event.stop_reason(), Some("end_turn"));
    }
}
