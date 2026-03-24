//! Streaming session support for ACP
//!
//! Converts Claude CLI stream-json output to ACP sessionUpdate notifications

use std::io::{BufRead, Write};
use std::process::{Command, Stdio};

use uuid::Uuid;

use super::types::{
    AcpResponse, SessionUpdate, SessionUpdateParams, MessageContent,
    ToolCallStatus, ToolCallContent, ToolKind, StopReason,
};
use super::stream::{StreamEvent, ContentBlock as StreamContentBlock};
use crate::agent::AgentInfo;

/// Streaming session state
pub struct StreamingSession {
    /// Session ID
    pub session_id: String,
    /// Agent info
    pub agent_info: AgentInfo,
    /// Working directory
    pub workdir: Option<String>,
    /// Claude session UUID (for --session-id)
    claude_session_uuid: String,
    /// Tool call counter (for generating tool call IDs)
    tool_call_counter: u32,
}

impl StreamingSession {
    /// Create a new streaming session
    pub fn new(agent_info: AgentInfo, workdir: Option<&str>) -> Self {
        let session_id = format!("sess_{}", Uuid::new_v4().simple());
        let claude_session_uuid = Uuid::new_v4().to_string();

        Self {
            session_id,
            agent_info,
            workdir: workdir.map(|s| s.to_string()),
            claude_session_uuid,
            tool_call_counter: 0,
        }
    }

    /// Generate next tool call ID
    fn next_tool_call_id(&mut self) -> String {
        self.tool_call_counter += 1;
        format!("tc_{}_{}", self.session_id, self.tool_call_counter)
    }

    /// Send a prompt and stream responses to the callback
    ///
    /// The callback receives ACP sessionUpdate notifications as NDJSON strings
    pub fn send_prompt_streaming<F>(&mut self, message: &str, mut on_update: F) -> Result<StopReason, String>
    where
        F: FnMut(&str) -> Result<(), String>,
    {
        use crate::config::AgentType;

        match self.agent_info.agent_type {
            AgentType::Claude => self.send_prompt_claude_streaming(message, on_update),
            AgentType::Process => self.send_prompt_process_streaming(message, on_update),
            AgentType::Builtin => Err("Builtin agents do not support streaming".to_string()),
        }
    }

    /// Send prompt to Claude CLI with streaming
    fn send_prompt_claude_streaming<F>(&mut self, message: &str, mut on_update: F) -> Result<StopReason, String>
    where
        F: FnMut(&str) -> Result<(), String>,
    {
        // Find claude command path
        let claude_path = if let Ok(path) = std::env::var("HOME") {
            let npm_path = format!("{}/.npm-global/bin/claude", path);
            if std::path::Path::new(&npm_path).exists() {
                npm_path
            } else {
                "claude".to_string()
            }
        } else {
            "claude".to_string()
        };

        let mut cmd = Command::new(&claude_path);
        cmd.arg("--print")
            .arg("--output-format")
            .arg("stream-json")
            .arg("--verbose")
            .arg("--session-id")
            .arg(&self.claude_session_uuid)
            .env_remove("CLAUDECODE");

        if let Some(ref dir) = self.workdir {
            cmd.current_dir(dir);
        }

        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd.spawn()
            .map_err(|e| format!("Failed to spawn claude: {}", e))?;

        // Write message to stdin
        if let Some(mut stdin) = child.stdin.take() {
            writeln!(stdin, "{}", message)
                .map_err(|e| format!("Write error: {}", e))?;
            stdin.flush()
                .map_err(|e| format!("Flush error: {}", e))?;
        }

        // Read streaming output
        let stdout = child.stdout.take().ok_or("Failed to get stdout")?;
        let reader = std::io::BufReader::new(stdout);

        let mut stop_reason = StopReason::EndTurn;

        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(e) => {
                    tracing::error!("Error reading line: {}", e);
                    continue;
                }
            };

            if line.trim().is_empty() {
                continue;
            }

            // Parse the stream event
            let event = match StreamEvent::from_line(&line) {
                Some(e) => e,
                None => continue,
            };

            // Convert to ACP notification
            match event {
                StreamEvent::System { subtype, .. } => {
                    tracing::debug!("System event: {}", subtype);
                }

                StreamEvent::Assistant { message, .. } => {
                    for block in message.content {
                        match block {
                            StreamContentBlock::Text { text } => {
                                // Send agent_message_chunk
                                let notification = self.make_message_chunk(&text);
                                on_update(&notification)?;
                            }
                            StreamContentBlock::Thinking { thinking, .. } => {
                                // Optionally send thinking as message chunk
                                let notification = self.make_message_chunk(&format!("[思考] {}", thinking));
                                on_update(&notification)?;
                            }
                            StreamContentBlock::ToolUse { id, name, input } => {
                                // Send tool_call notification
                                let tool_call_id = format!("tc_{}", id);
                                let (kind, title) = self.parse_tool_info(&name, &input);

                                let notification = self.make_tool_call(&tool_call_id, &title, &kind);
                                on_update(&notification)?;

                                // Immediately send in_progress status
                                // (tool execution is synchronous in this implementation)
                            }
                            StreamContentBlock::ToolResult { tool_use_id, content, is_error } => {
                                // Send tool_call_update notification
                                let tool_call_id = format!("tc_{}", tool_use_id);
                                let status = if is_error.unwrap_or(false) {
                                    ToolCallStatus::Failed
                                } else {
                                    ToolCallStatus::Completed
                                };

                                let notification = self.make_tool_call_update(&tool_call_id, status, content.as_deref());
                                on_update(&notification)?;
                            }
                        }
                    }
                }

                StreamEvent::Result { stop_reason: sr, .. } => {
                    // Map stop reason
                    if let Some(sr) = sr {
                        stop_reason = match sr.as_str() {
                            "end_turn" => StopReason::EndTurn,
                            "stop" => StopReason::Stop,
                            "cancelled" => StopReason::Cancelled,
                            "refusal" => StopReason::Refusal,
                            "error" => StopReason::Error,
                            _ => StopReason::EndTurn,
                        };
                    }
                }

                StreamEvent::User { .. } => {}
            }
        }

        // Wait for process to complete
        let status = child.wait()
            .map_err(|e| format!("Wait error: {}", e))?;

        if !status.success() {
            // Read stderr for error message
            if let Some(stderr) = child.stderr.take() {
                let stderr_reader = std::io::BufReader::new(stderr);
                let error_msg: String = stderr_reader.lines()
                    .filter_map(|l| l.ok())
                    .collect::<Vec<_>>()
                    .join("\n");
                if !error_msg.is_empty() {
                    return Err(format!("Claude error: {}", error_msg));
                }
            }
            return Err("Claude process failed".to_string());
        }

        Ok(stop_reason)
    }

    /// Send prompt to external process (non-streaming for now)
    fn send_prompt_process_streaming<F>(&mut self, message: &str, mut on_update: F) -> Result<StopReason, String>
    where
        F: FnMut(&str) -> Result<(), String>,
    {
        if self.agent_info.command.is_empty() {
            return Err(format!("Agent {} has no command configured", self.agent_info.id));
        }

        let mut cmd = Command::new(&self.agent_info.command);

        for arg in &self.agent_info.args {
            cmd.arg(arg);
        }

        cmd.arg(message);

        if let Some(ref dir) = self.workdir {
            cmd.current_dir(dir);
        } else if let Some(ref dir) = self.agent_info.working_dir {
            cmd.current_dir(dir);
        }

        for (key, value) in &self.agent_info.env {
            cmd.env(key, value);
        }

        let output = cmd.output()
            .map_err(|e| format!("Failed to execute {}: {}", self.agent_info.command, e))?;

        if output.status.success() {
            let result = String::from_utf8_lossy(&output.stdout);

            // Send as a single message chunk
            if !result.is_empty() {
                let notification = self.make_message_chunk(&result);
                on_update(&notification)?;
            }

            Ok(StopReason::EndTurn)
        } else {
            let error = String::from_utf8_lossy(&output.stderr);
            Err(format!("Process error: {}", error))
        }
    }

    /// Create agent_message_chunk notification
    fn make_message_chunk(&self, text: &str) -> String {
        let notification = SessionUpdateParams {
            sessionId: self.session_id.clone(),
            update: SessionUpdate::AgentMessageChunk {
                content: MessageContent::Text { text: text.to_string() },
            },
        };

        serde_json::to_string(&AcpResponse::notification("sessionUpdate", notification))
            .unwrap_or_else(|_| "{}".to_string())
    }

    /// Create tool_call notification
    fn make_tool_call(&self, tool_call_id: &str, title: &str, kind: &Option<ToolKind>) -> String {
        let notification = SessionUpdateParams {
            sessionId: self.session_id.clone(),
            update: SessionUpdate::ToolCall {
                toolCallId: tool_call_id.to_string(),
                title: title.to_string(),
                status: ToolCallStatus::InProgress,
                rawInput: None,
                kind: kind.clone(),
            },
        };

        serde_json::to_string(&AcpResponse::notification("sessionUpdate", notification))
            .unwrap_or_else(|_| "{}".to_string())
    }

    /// Create tool_call_update notification
    fn make_tool_call_update(&self, tool_call_id: &str, status: ToolCallStatus, output: Option<&str>) -> String {
        let content = output.map(|o| vec![
            ToolCallContent::Content {
                content: MessageContent::Text { text: o.to_string() },
            }
        ]);

        let notification = SessionUpdateParams {
            sessionId: self.session_id.clone(),
            update: SessionUpdate::ToolCallUpdate {
                toolCallId: tool_call_id.to_string(),
                status: Some(status),
                rawOutput: output.map(|o| serde_json::json!(o)),
                content,
            },
        };

        serde_json::to_string(&AcpResponse::notification("sessionUpdate", notification))
            .unwrap_or_else(|_| "{}".to_string())
    }

    /// Parse tool info from name and input
    fn parse_tool_info(&self, name: &str, input: &serde_json::Value) -> (Option<ToolKind>, String) {
        let kind = match name {
            "Read" | "NBRead" => Some(ToolKind::Read),
            "Edit" | "NBEdit" => Some(ToolKind::Edit),
            "Write" | "NBWrite" => Some(ToolKind::Other), // Write is not in ToolKind
            "Delete" => Some(ToolKind::Delete),
            "Glob" | "Grep" => Some(ToolKind::Search),
            "Bash" => Some(ToolKind::Execute),
            "WebFetch" => Some(ToolKind::Fetch),
            _ => Some(ToolKind::Other),
        };

        // Create a readable title
        let title = match name {
            "Read" => {
                let path = input.get("file_path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                format!("read: {}", path)
            }
            "Edit" => {
                let path = input.get("file_path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                format!("edit: {}", path)
            }
            "Write" => {
                let path = input.get("file_path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                format!("write: {}", path)
            }
            "Bash" => {
                let cmd = input.get("command")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                // Truncate long commands
                let truncated = if cmd.len() > 50 {
                    format!("{}...", &cmd[..47])
                } else {
                    cmd.to_string()
                };
                format!("exec: {}", truncated)
            }
            "Glob" => {
                let pattern = input.get("pattern")
                    .and_then(|v| v.as_str())
                    .unwrap_or("*");
                format!("glob: {}", pattern)
            }
            "Grep" => {
                let pattern = input.get("pattern")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                format!("grep: {}", pattern)
            }
            _ => name.to_string(),
        };

        (kind, title)
    }
}
