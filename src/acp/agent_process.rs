//! AcpAgentProcess — persistent Claude CLI process for process-per-session model
//!
//! Manages a single Claude CLI process using the stream-json protocol:
//! - Claude CLI: --print --output-format stream-json --input-format stream-json
//! - stdin: write user messages as stream-json {"type":"user","message":{...}}
//! - stdout: read streaming JSON events
//!
//! Architecture: AcpAgentProcess holds child process and event receiver.
//! A background task reads stdout events and sends them via mpsc channel.
//! The process starts in an idle state waiting for prompts.
//! Each prompt() sends a message via stdin and reads events from the channel.

use std::process::Stdio as StdStdio;

use tokio::io::{AsyncWriteExt, BufReader, BufWriter};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, oneshot};

use super::streaming::StreamingEvent;
use super::types::StopReason;

/// Raw event from stdout reader
#[derive(Debug)]
pub enum RawEvent {
    Line(String),
    Eof,
}

/// Claude CLI stream-json output event types
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(tag = "type")]
enum ClaudeEvent {
    #[serde(rename = "system")]
    System {
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
        #[serde(default)]
        is_error: Option<bool>,
    },
    #[serde(rename = "error")]
    Error {
        #[serde(default)]
        error: Option<String>,
        #[serde(default)]
        session_id: Option<String>,
    },
}

/// Assistant message content blocks
#[derive(Debug, Clone, serde::Deserialize)]
struct AssistantMessage {
    #[serde(default)]
    content: Vec<ContentBlock>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(tag = "type")]
enum ContentBlock {
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

/// ACP Agent Process — persistent Claude CLI process communicating via stream-json
pub struct AcpAgentProcess {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    /// Claude CLI session ID (for --resume on subsequent prompts)
    claude_session_id: String,
    /// Receiver for stdout events from background reader task
    event_rx: mpsc::Receiver<RawEvent>,
    /// Flag: background reader still active
    alive: bool,
}

impl AcpAgentProcess {
    /// Spawn a new Claude CLI process with stream-json protocol
    ///
    /// Spawns Claude CLI with piped stdin/stdout, starts background stdout reader,
    /// and immediately sends a dummy prompt to trigger the init event.
    pub async fn spawn(
        command: &str,
        args: &[String],
        env: &std::collections::HashMap<String, String>,
        env_remove: &[String],
        workdir: Option<&str>,
    ) -> Result<Self, String> {
        let mut cmd = Command::new(command);
        cmd.args(args)
            .stdin(StdStdio::piped())
            .stdout(StdStdio::piped())
            .stderr(StdStdio::piped());

        for key in env_remove {
            cmd.env_remove(key);
        }
        for (key, value) in env {
            cmd.env(key, value);
        }
        if let Some(dir) = workdir {
            cmd.current_dir(dir);
        }

        let mut child = cmd.spawn().map_err(|e| format!("Failed to spawn {}: {}", command, e))?;
        let stdin = child.stdin.take().ok_or("Failed to get stdin")?;
        let stdout = child.stdout.take().ok_or("Failed to get stdout")?;
        let stderr = child.stderr.take();

        // Log stderr in background
        if let Some(stderr) = stderr {
            tokio::spawn(async move {
                use tokio::io::AsyncBufReadExt;
                let reader = BufReader::new(stderr);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!("Claude stderr: {}", line);
                }
            });
        }

        // Create channel for stdout reader
        let (event_tx, event_rx) = mpsc::channel::<RawEvent>(64);

        // Spawn background stdout reader
        tokio::spawn(stdout_reader_task(stdout, event_tx));

        let mut process = Self {
            child,
            stdin: BufWriter::new(stdin),
            claude_session_id: String::new(),
            event_rx,
            alive: true,
        };

        // Send a dummy prompt to trigger the init event
        // This gets the session_id and puts Claude in a ready state
        let dummy_msg = serde_json::json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": "echo ok"
            }
        });
        let dummy_line = serde_json::to_string(&dummy_msg).map_err(|e| e.to_string())?;
        process.stdin.write_all(dummy_line.as_bytes()).await
            .map_err(|e| format!("Write error: {}", e))?;
        process.stdin.write_all(b"\n").await
            .map_err(|e| format!("Write error: {}", e))?;
        process.stdin.flush().await
            .map_err(|e| format!("Flush error: {}", e))?;

        // Drain init events from stdout to get session_id and discard dummy response
        process.drain_init_events().await?;

        tracing::info!("Claude CLI spawned, session_id: {}", process.claude_session_id);
        Ok(process)
    }

    /// Drain events until we get the init session_id and the dummy response
    async fn drain_init_events(&mut self) -> Result<(), String> {
        // Drain events until we see a result (end of first response)
        let mut found_session_id = false;
        let mut found_result = false;

        while let Some(raw) = self.event_rx.recv().await {
            match raw {
                RawEvent::Line(line) => {
                    if let Some(sid) = extract_session_id(&line) {
                        self.claude_session_id = sid.clone();
                        found_session_id = true;
                        tracing::debug!("Got session_id: {}", sid);
                    }
                    // Check if this is a result event
                    if let Ok(event) = serde_json::from_str::<serde_json::Value>(&line) {
                        if event.get("type").and_then(|t| t.as_str()) == Some("result") {
                            found_result = true;
                            tracing::debug!("Got result (init complete)");
                        }
                    }
                    if found_session_id && found_result {
                        break;
                    }
                }
                RawEvent::Eof => {
                    return Err("Claude CLI closed stdout during init".to_string());
                }
            }
        }

        if !found_session_id {
            return Err("Failed to get session_id from Claude CLI".to_string());
        }

        Ok(())
    }

    /// Send a prompt and stream responses via channel.
    ///
    /// Sends the message in stream-json format via stdin.
    /// Reads stdout events and forwards as StreamingEvent notifications.
    pub async fn prompt(
        &mut self,
        message: &str,
        tx: mpsc::Sender<StreamingEvent>,
    ) -> Result<(), String> {
        // Send message in stream-json format
        let json_msg = serde_json::json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": message
            }
        });
        let line = serde_json::to_string(&json_msg).map_err(|e| format!("JSON encode error: {}", e))?;
        self.stdin.write_all(line.as_bytes()).await
            .map_err(|e| format!("Write error: {}", e))?;
        self.stdin.write_all(b"\n").await
            .map_err(|e| format!("Write newline error: {}", e))?;
        self.stdin.flush().await
            .map_err(|e| format!("Flush error: {}", e))?;

        tracing::debug!("Sent prompt via stream-json");

        let mut accumulated_content = String::new();
        let mut stop_reason = StopReason::EndTurn;

        // Read events from the background reader
        while let Some(raw) = self.event_rx.recv().await {
            match raw {
                RawEvent::Line(line) => {
                    match classify_event(&line) {
                        EventKind::MessageChunk(text) => {
                            accumulated_content.push_str(&text);
                            let notification = make_chunk_notification(&self.claude_session_id, &text);
                            if tx.send(StreamingEvent::Notification(notification)).await.is_err() {
                                tracing::warn!("Client disconnected");
                                return Ok(());
                            }
                        }
                        EventKind::ToolUse(id, name, input) => {
                            let tool_call_id = format!("tc_{}", id);
                            let title = format_tool_title(&name, &Some(input.clone()));
                            let kind = infer_tool_kind(&name);
                            let notification = make_tool_notification(&self.claude_session_id, &tool_call_id, &title, kind.as_ref());
                            if tx.send(StreamingEvent::Notification(notification)).await.is_err() {
                                tracing::warn!("Client disconnected");
                                return Ok(());
                            }
                        }
                        EventKind::ToolResult(id, content, is_error) => {
                            let tool_call_id = format!("tc_{}", id);
                            let status = if is_error { super::types::ToolCallStatus::Failed } else { super::types::ToolCallStatus::Completed };
                            let output_text = content.as_ref().and_then(|c| c.as_str());
                            let notification = make_tool_update_notification(&self.claude_session_id, &tool_call_id, status, output_text);
                            if tx.send(StreamingEvent::Notification(notification)).await.is_err() {
                                tracing::warn!("Client disconnected");
                                return Ok(());
                            }
                        }
                        EventKind::Assistant(message) => {
                            // Extract tool uses from assistant message content blocks
                            if let Some(msg) = message {
                                for block in msg.content {
                                    match block {
                                        ContentBlock::ToolUse { id, name, input } => {
                                            let tool_call_id = format!("tc_{}", id);
                                            let title = format_tool_title(&name, &Some(input.clone()));
                                            let kind = infer_tool_kind(&name);
                                            let notification = make_tool_notification(&self.claude_session_id, &tool_call_id, &title, kind.as_ref());
                                            if tx.send(StreamingEvent::Notification(notification)).await.is_err() {
                                                tracing::warn!("Client disconnected");
                                                return Ok(());
                                            }
                                        }
                                        ContentBlock::ToolResultBlock { tool_use_id, content } => {
                                            let tool_call_id = format!("tc_{}", tool_use_id);
                                            let output_text = content.as_ref().and_then(|c| c.as_str());
                                            let notification = make_tool_update_notification(
                                                &self.claude_session_id, &tool_call_id,
                                                super::types::ToolCallStatus::Completed, output_text
                                            );
                                            if tx.send(StreamingEvent::Notification(notification)).await.is_err() {
                                                tracing::warn!("Client disconnected");
                                                return Ok(());
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                        EventKind::System(session_id) => {
                            if let Some(sid) = session_id {
                                if self.claude_session_id.is_empty() {
                                    self.claude_session_id = sid;
                                }
                            }
                        }
                        EventKind::Result(result, stop_reason_str, is_error) => {
                            if is_error {
                                let err_msg = result.unwrap_or_default();
                                let _ = tx.send(StreamingEvent::Completed(
                                    super::streaming::AsyncStreamingResult::Error(err_msg)
                                )).await;
                                return Ok(());
                            }
                            if let Some(ref sr) = stop_reason_str {
                                stop_reason = match sr.as_str() {
                                    "end_turn" => StopReason::EndTurn,
                                    "stop" => StopReason::Stop,
                                    _ => StopReason::EndTurn,
                                };
                            }
                            if let Some(text) = result {
                                if accumulated_content.is_empty() {
                                    accumulated_content = text;
                                }
                            }
                        }
                        EventKind::Error(msg) => {
                            tracing::warn!("Claude error: {}", msg);
                        }
                        EventKind::Ignore => {}
                    }
                }
                RawEvent::Eof => {
                    tracing::debug!("Claude stdout EOF during prompt");
                    self.alive = false;
                    break;
                }
            }
        }

        let result = super::streaming::AsyncStreamingResult::Completed {
            stop_reason,
            content: accumulated_content,
            claude_session_id: Some(self.claude_session_id.clone()),
        };
        if tx.send(StreamingEvent::Completed(result)).await.is_err() {
            tracing::warn!("Client disconnected before completion");
        }
        Ok(())
    }

    /// Cancel the current prompt
    #[allow(unused_variables)]
    pub async fn cancel(&mut self) -> Result<(), String> {
        tracing::info!("Cancel requested for session {}", self.claude_session_id);
        // Note: Claude CLI doesn't have a clean cancel mechanism in stream-json mode.
        // The best we can do is let the process continue and ignore the result.
        Ok(())
    }

    /// Kill the child process
    pub async fn close(&mut self) {
        let _ = self.child.kill().await;
        tracing::info!("ACP agent process closed");
    }

    /// Get the Claude CLI session ID
    pub fn claude_session_id(&self) -> &str {
        &self.claude_session_id
    }
}

// ============================================================================
// Background stdout reader
// ============================================================================

/// Background task that reads stdout lines and sends them via channel
async fn stdout_reader_task(stdout: tokio::process::ChildStdout, tx: mpsc::Sender<RawEvent>) {
    use tokio::io::AsyncBufReadExt;

    let reader = BufReader::new(stdout);
    let mut lines = reader.lines();

    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }

        if tx.send(RawEvent::Line(line)).await.is_err() {
            // Receiver dropped, stop reading
            break;
        }
    }

    let _ = tx.send(RawEvent::Eof).await;
}

// ============================================================================
// Event classification
// ============================================================================

#[derive(Debug)]
enum EventKind {
    MessageChunk(String),
    ToolUse(String, String, serde_json::Value),
    ToolResult(String, Option<serde_json::Value>, bool),
    Assistant(Option<AssistantMessage>),
    System(Option<String>),
    Result(Option<String>, Option<String>, bool),
    Error(String),
    Ignore,
}

fn classify_event(line: &str) -> EventKind {
    let event: serde_json::Value = match serde_json::from_str(line) {
        Ok(e) => e,
        Err(_) => return EventKind::Ignore,
    };

    let event_type = event.get("type").and_then(|t| t.as_str()).unwrap_or("");

    match event_type {
        "system" => {
            let session_id = event.get("session_id")
                .and_then(|s| s.as_str())
                .map(String::from);
            EventKind::System(session_id)
        }
        "assistant" => {
            let msg: Option<AssistantMessage> = serde_json::from_value(
                event.get("message").cloned().unwrap_or(serde_json::Value::Null)
            ).ok();
            EventKind::Assistant(msg)
        }
        "result" => {
            let is_error = event.get("is_error").and_then(|e| e.as_bool()).unwrap_or(false);
            let result = event.get("result").and_then(|r| r.as_str()).map(String::from);
            let stop_reason = event.get("stop_reason").and_then(|s| s.as_str()).map(String::from);
            EventKind::Result(result, stop_reason, is_error)
        }
        "tool_use" => {
            let id = event.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
            let name = event.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
            let input = event.get("input").cloned().unwrap_or(serde_json::Value::Null);
            EventKind::ToolUse(id, name, input)
        }
        "tool_result" => {
            let id = event.get("tool_use_id").and_then(|i| i.as_str()).unwrap_or("").to_string();
            let content = event.get("content").cloned();
            let is_error = event.get("is_error").and_then(|e| e.as_bool()).unwrap_or(false);
            EventKind::ToolResult(id, content, is_error)
        }
        "error" => {
            let msg = event.get("error").and_then(|e| e.as_str()).unwrap_or("Unknown error").to_string();
            EventKind::Error(msg)
        }
        _ => {
            // Check for text content in any event
            if let Some(text) = event.get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_str())
            {
                if !text.is_empty() {
                    return EventKind::MessageChunk(text.to_string());
                }
            }
            EventKind::Ignore
        }
    }
}

/// Extract session_id from a raw JSON line
fn extract_session_id(line: &str) -> Option<String> {
    let event: serde_json::Value = match serde_json::from_str(line) {
        Ok(e) => e,
        Err(_) => return None,
    };
    if event.get("type").and_then(|t| t.as_str()) == Some("system") {
        event.get("session_id")
            .and_then(|s| s.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from)
    } else {
        None
    }
}

// ============================================================================
// Notification builders
// ============================================================================

use super::types::{AcpResponse, MessageContent, SessionUpdate, SessionUpdateParams, ToolCallStatus, ToolKind, ToolCallContent};

fn make_chunk_notification(session_id: &str, text: &str) -> String {
    let notification = SessionUpdateParams {
        sessionId: session_id.to_string(),
        update: SessionUpdate::AgentMessageChunk {
            content: MessageContent::Text { text: text.to_string() },
        },
    };
    serde_json::to_string(&AcpResponse::notification("sessionUpdate", notification))
        .unwrap_or_else(|_| "{}".to_string())
}

fn make_tool_notification(session_id: &str, tool_call_id: &str, title: &str, kind: Option<&ToolKind>) -> String {
    let notification = SessionUpdateParams {
        sessionId: session_id.to_string(),
        update: SessionUpdate::ToolCall {
            toolCallId: tool_call_id.to_string(),
            title: title.to_string(),
            status: ToolCallStatus::InProgress,
            rawInput: None,
            kind: kind.cloned(),
        },
    };
    serde_json::to_string(&AcpResponse::notification("sessionUpdate", notification))
        .unwrap_or_else(|_| "{}".to_string())
}

fn make_tool_update_notification(session_id: &str, tool_call_id: &str, status: ToolCallStatus, output: Option<&str>) -> String {
    let content = output.map(|o| vec![
        ToolCallContent::Content {
            content: MessageContent::Text { text: o.to_string() },
        }
    ]);
    let notification = SessionUpdateParams {
        sessionId: session_id.to_string(),
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

fn format_tool_title(name: &str, input: &Option<serde_json::Value>) -> String {
    match name {
        "Read" | "NBRead" => {
            let path = input.as_ref()
                .and_then(|i| i.get("file_path"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            format!("Read({})", path)
        }
        "Edit" | "NBEdit" => {
            let path = input.as_ref()
                .and_then(|i| i.get("file_path"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            format!("Edit({})", path)
        }
        "Write" | "NBWrite" => {
            let path = input.as_ref()
                .and_then(|i| i.get("file_path"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            format!("Write({})", path)
        }
        "Delete" => {
            let path = input.as_ref()
                .and_then(|i| i.get("file_path"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            format!("Delete({})", path)
        }
        "Bash" => {
            let cmd = input.as_ref()
                .and_then(|i| i.get("command"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let truncated = if cmd.chars().count() > 40 {
                format!("{}...", cmd.chars().take(37).collect::<String>())
            } else {
                cmd.to_string()
            };
            format!("Bash({})", truncated)
        }
        "Glob" => {
            let pattern = input.as_ref()
                .and_then(|i| i.get("pattern"))
                .and_then(|v| v.as_str())
                .unwrap_or("*");
            format!("Glob({})", pattern)
        }
        "Grep" => {
            let pattern = input.as_ref()
                .and_then(|i| i.get("pattern"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!("Grep({})", pattern)
        }
        _ => name.to_string(),
    }
}

fn infer_tool_kind(name: &str) -> Option<ToolKind> {
    match name {
        "Read" | "NBRead" => Some(ToolKind::Read),
        "Edit" | "NBEdit" => Some(ToolKind::Edit),
        "Write" | "NBWrite" => Some(ToolKind::Other),
        "Delete" => Some(ToolKind::Delete),
        "Glob" | "Grep" => Some(ToolKind::Search),
        "Bash" => Some(ToolKind::Execute),
        "WebFetch" => Some(ToolKind::Fetch),
        _ => Some(ToolKind::Other),
    }
}
