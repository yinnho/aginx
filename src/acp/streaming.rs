//! Async streaming session support for ACP with Claude CLI JSON output parsing
//!
//! Uses Claude CLI with --output-format stream-json --include-partial-messages
//! for real-time token-by-token streaming via tokio::process::Command.

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use super::types::{
    AcpResponse, SessionUpdate, SessionUpdateParams,
    MessageContent, ToolCallStatus, ToolCallContent, ToolKind, StopReason,
};
use crate::agent::AgentInfo;

/// Claude CLI stream-json output event
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(tag = "type")]
enum ClaudeEvent {
    #[serde(rename = "system")]
    System {
        session_id: Option<String>,
    },
    #[serde(rename = "assistant")]
    Assistant {
        message: Option<AssistantMessage>,
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
        content: Option<serde_json::Value>,
        #[serde(rename = "is_error")]
        is_error: Option<bool>,
    },
    #[serde(rename = "result")]
    Result {
        result: Option<String>,
        stop_reason: Option<String>,
        session_id: Option<String>,
        is_error: Option<bool>,
    },
}

/// Inner event from stream_event (Anthropic API streaming format)
#[derive(Debug, Clone, serde::Deserialize)]
struct StreamInnerEvent {
    #[serde(rename = "type")]
    event_type: String,
    delta: Option<StreamDelta>,
}

/// Delta from stream_event
#[derive(Debug, Clone, serde::Deserialize)]
struct StreamDelta {
    #[serde(rename = "type")]
    delta_type: String,
    #[serde(default)]
    text: String,
    #[serde(rename = "stop_reason")]
    stop_reason: Option<String>,
}

/// Assistant message structure from Claude CLI
#[derive(Debug, Clone, serde::Deserialize)]
struct AssistantMessage {
    content: Vec<ContentBlock>,
}

/// Content block in assistant message
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
        content: Option<serde_json::Value>,
    },
}

/// Async streaming result (sent through channel when complete)
#[derive(Debug, Clone)]
pub enum AsyncStreamingResult {
    /// Completed successfully
    Completed {
        stop_reason: StopReason,
        content: String,
        claude_session_id: Option<String>,
    },
    /// Error occurred
    Error(String),
}

/// Streaming event (for async channel communication)
#[derive(Debug, Clone)]
pub enum StreamingEvent {
    /// ACP sessionUpdate notification JSON
    Notification(String),
    /// Streaming completed
    Completed(AsyncStreamingResult),
}

/// Streaming session (async only, no state held)
pub struct StreamingSession;

impl StreamingSession {
    /// Run Claude CLI asynchronously with tokio::process::Command
    ///
    /// Returns a receiver that yields streaming events in real-time.
    pub async fn run_claude_async(
        session_id: String,
        agent_info: AgentInfo,
        workdir: Option<String>,
        claude_session_id: Option<String>,
        message: &str,
    ) -> Result<mpsc::Receiver<StreamingEvent>, String> {
        use std::process::Stdio;

        // Build args: base args + session resume args (no message, it's sent via stdin)
        let mut full_args: Vec<String> = agent_info.args.clone();

        if let Some(ref sid) = claude_session_id {
            for arg in &agent_info.session_args {
                full_args.push(arg.replace("${SESSION_ID}", sid));
            }
        }
        // Message will be sent via stdin

        tracing::info!("[STREAM] Spawning async: {} {:?}", agent_info.command, full_args);

        let (tx, rx) = mpsc::channel::<StreamingEvent>(64);

        let mut cmd = Command::new(&agent_info.command);
        cmd.args(&full_args);

        for (key, value) in &agent_info.env {
            cmd.env(key, value);
        }
        for env_key in &agent_info.env_remove {
            cmd.env_remove(env_key);
        }
        if let Some(ref dir) = workdir {
            cmd.current_dir(dir);
        }

        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.stdin(Stdio::piped());

        let mut child = cmd.spawn()
            .map_err(|e| format!("Failed to spawn {}: {}", agent_info.command, e))?;

        let stdout = child.stdout.take().ok_or("Failed to capture stdout")?;
        let stderr = child.stderr.take().ok_or("Failed to capture stderr")?;
        let stdin = child.stdin.take().ok_or("Failed to capture stdin")?;

        // Write message to stdin in stream-json format
        let json_msg = serde_json::json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": message
            }
        });
        let line = serde_json::to_string(&json_msg).map_err(|e| format!("JSON encode error: {}", e))?;
        let line_owned = line;
        tokio::spawn(async move {
            let mut stdin = stdin;
            if let Err(e) = stdin.write_all(line_owned.as_bytes()).await {
                tracing::warn!("Failed to write to stdin: {}", e);
            }
            if let Err(e) = stdin.write_all(b"\n").await {
                tracing::warn!("Failed to write newline to stdin: {}", e);
            }
            if let Err(e) = stdin.flush().await {
                tracing::warn!("Failed to flush stdin: {}", e);
            }
            if let Err(e) = stdin.shutdown().await {
                tracing::warn!("Failed to shutdown stdin: {}", e);
            }
        });

        let session_id_clone = session_id.clone();
        let tx_clone = tx.clone();

        // Process stdout: parse JSON lines, send notifications
        let stdout_task = tokio::spawn(async move {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            let mut accumulated_content = String::new();
            let mut claude_session_id: Option<String> = None;
            let mut stop_reason = StopReason::EndTurn;

            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }

                tracing::debug!("[STREAM] Async line: {}", &line[..line.len().min(200)]);

                match process_line(
                    &session_id_clone,
                    &line,
                    &mut accumulated_content,
                    &mut claude_session_id,
                    &mut stop_reason,
                ) {
                    Ok(Some(notification)) => {
                        if tx_clone.send(StreamingEvent::Notification(notification)).await.is_err() {
                            tracing::warn!("Client disconnected, stopping stdout task");
                            break;
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        tracing::warn!("Error processing line: {}", e);
                    }
                }
            }

            let result = AsyncStreamingResult::Completed {
                stop_reason,
                content: accumulated_content,
                claude_session_id,
            };
            let _ = tx_clone.send(StreamingEvent::Completed(result)).await;
        });

        // Log stderr
        let stderr_task = tokio::spawn(async move {
            let reader = BufReader::new(stderr);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::warn!("Claude stderr: {}", line);
            }
        });

        // Wait for process
        tokio::spawn(async move {
            let _ = stdout_task.await;
            let _ = stderr_task.await;
            match child.wait().await {
                Ok(status) => tracing::info!("Process completed: {:?}", status),
                Err(e) => tracing::error!("Wait error: {}", e),
            }
        });

        Ok(rx)
    }
}

/// Process a single JSON line and return notification if any
fn process_line(
    session_id: &str,
    line: &str,
    accumulated_content: &mut String,
    claude_session_id: &mut Option<String>,
    stop_reason: &mut StopReason,
) -> Result<Option<String>, String> {
    let event: ClaudeEvent = match serde_json::from_str(line) {
        Ok(e) => e,
        Err(_) => return Ok(None),
    };

    match event {
        ClaudeEvent::System { session_id: sid } => {
            if let Some(sid) = sid {
                tracing::info!("Got Claude session_id from init: {}", sid);
                *claude_session_id = Some(sid);
            }
            Ok(None)
        }

        ClaudeEvent::Assistant { message, session_id: sid } => {
            if let Some(sid) = sid {
                if claude_session_id.is_none() {
                    *claude_session_id = Some(sid);
                }
            }
            let mut last_notification: Option<String> = None;
            if let Some(msg) = message {
                for block in msg.content {
                    let notification = match block {
                        ContentBlock::Text { text } => {
                            // Stream events already sent text via text_delta chunks.
                            // Skip to avoid duplication.
                            let _ = text;
                            None
                        }
                        ContentBlock::Thinking { thinking } => {
                            tracing::debug!("Thinking: {} chars", thinking.len());
                            None
                        }
                        ContentBlock::ToolUse { id, name, input } => {
                            let tool_call_id = format!("tc_{}", id);
                            let title = format_tool_title(&name, &Some(input.clone()));
                            let kind = infer_tool_kind(&name);
                            Some(make_tool_call_notification(session_id, &tool_call_id, &title, &kind))
                        }
                        ContentBlock::ToolResultBlock { tool_use_id, content } => {
                            let tool_call_id = format!("tc_{}", tool_use_id);
                            let output_text = content.as_ref().and_then(|c| c.as_str());
                            Some(make_tool_call_update_notification(
                                session_id,
                                &tool_call_id,
                                ToolCallStatus::Completed,
                                output_text,
                            ))
                        }
                    };
                    if notification.is_some() {
                        last_notification = notification;
                    }
                }
            }
            Ok(last_notification)
        }

        ClaudeEvent::ToolUse { id, name, input } => {
            let tool_call_id = format!("tc_{}", id);
            let title = format_tool_title(&name, &Some(input.clone()));
            let kind = infer_tool_kind(&name);
            Ok(Some(make_tool_call_notification(session_id, &tool_call_id, &title, &kind)))
        }

        ClaudeEvent::ToolResult { tool_use_id, content, is_error } => {
            let tool_call_id = format!("tc_{}", tool_use_id);
            let status = if is_error.unwrap_or(false) {
                ToolCallStatus::Failed
            } else {
                ToolCallStatus::Completed
            };
            let output_text = content.as_ref().and_then(|c| c.as_str());
            Ok(Some(make_tool_call_update_notification(session_id, &tool_call_id, status, output_text)))
        }

        ClaudeEvent::StreamEvent { inner } => {
            match inner.event_type.as_str() {
                "content_block_delta" => {
                    if let Some(delta) = inner.delta {
                        tracing::info!("[STREAM] content_block_delta: type={}, text_len={}", delta.delta_type, delta.text.len());
                        if delta.delta_type == "text_delta" && !delta.text.is_empty() {
                            accumulated_content.push_str(&delta.text);
                            return Ok(Some(make_message_chunk_notification(session_id, &delta.text)));
                        }
                    }
                }
                "message_delta" => {
                    if let Some(delta) = inner.delta {
                        if let Some(ref sr) = delta.stop_reason {
                            *stop_reason = match sr.as_str() {
                                "end_turn" => StopReason::EndTurn,
                                "stop" => StopReason::Stop,
                                "tool_use" => StopReason::EndTurn,
                                _ => StopReason::EndTurn,
                            };
                        }
                    }
                }
                _ => {}
            }
            Ok(None)
        }

        ClaudeEvent::Result { result, stop_reason: reason, session_id: sid, is_error } => {
            if let Some(sid) = sid {
                tracing::info!("Got Claude session_id from result: {}", sid);
                *claude_session_id = Some(sid);
            }

            if let Some(text) = result {
                tracing::info!("[STREAM] Result event, text_len={}, accumulated_empty={}", text.len(), accumulated_content.is_empty());
                if !text.is_empty() && accumulated_content.is_empty() {
                    accumulated_content.push_str(&text);
                    return Ok(Some(make_message_chunk_notification(session_id, &text)));
                }
                if is_error.unwrap_or(false) {
                    return Err(format!("Claude error: {}", text));
                }
            }

            *stop_reason = match reason.as_deref() {
                Some("end_turn") => StopReason::EndTurn,
                Some("stop") => StopReason::Stop,
                Some("tool_use") => StopReason::EndTurn,
                _ => StopReason::EndTurn,
            };

            Ok(None)
        }
    }
}

/// Create agent_message_chunk notification
fn make_message_chunk_notification(session_id: &str, text: &str) -> String {
    let notification = SessionUpdateParams {
        sessionId: session_id.to_string(),
        update: SessionUpdate::AgentMessageChunk {
            content: MessageContent::Text { text: text.to_string() },
        },
    };
    serde_json::to_string(&AcpResponse::notification("sessionUpdate", notification))
        .unwrap_or_else(|_| "{}".to_string())
}

/// Create tool_call notification
fn make_tool_call_notification(session_id: &str, tool_call_id: &str, title: &str, kind: &Option<ToolKind>) -> String {
    let notification = SessionUpdateParams {
        sessionId: session_id.to_string(),
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
fn make_tool_call_update_notification(session_id: &str, tool_call_id: &str, status: ToolCallStatus, output: Option<&str>) -> String {
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

/// Format tool title for display
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

/// Infer tool kind from name
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
