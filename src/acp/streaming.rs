//! Async streaming session support for ACP with Claude CLI JSON output parsing
//!
//! Uses Claude CLI with --output-format stream-json --include-partial-messages
//! for real-time token-by-token streaming via tokio::process::Command.

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use super::types::{StopReason, ToolCallStatus};
use super::agent_event::{
    format_tool_title, infer_tool_kind,
    make_chunk_notification, make_tool_notification, make_tool_update_notification,
    AgentEvent, ContentBlock,
};
use crate::agent::AgentInfo;

/// Async streaming result (sent through channel when complete)
#[derive(Debug, Clone)]
pub enum AsyncStreamingResult {
    /// Completed successfully
    Completed {
        stop_reason: StopReason,
        content: String,
        agent_session_id: Option<String>,
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
    pub async fn run_streaming_async(
        session_id: String,
        agent_info: AgentInfo,
        workdir: Option<String>,
        agent_session_id: Option<String>,
        message: &str,
    ) -> Result<mpsc::Receiver<StreamingEvent>, String> {
        use std::process::Stdio;

        // Build args: base args + session resume args (no message, it's sent via stdin)
        let mut full_args: Vec<String> = agent_info.args.clone();

        if let Some(ref sid) = agent_session_id {
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
            let mut agent_session_id: Option<String> = None;
            let mut stop_reason = StopReason::EndTurn;

            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }

                tracing::debug!("[STREAM] Async line: {}", line.chars().take(200).collect::<String>());

                match process_line(
                    &session_id_clone,
                    &line,
                    &mut accumulated_content,
                    &mut agent_session_id,
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
                agent_session_id,
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
    agent_session_id: &mut Option<String>,
    stop_reason: &mut StopReason,
) -> Result<Option<String>, String> {
    let event: AgentEvent = match serde_json::from_str(line) {
        Ok(e) => e,
        Err(_) => return Ok(None),
    };

    match event {
        AgentEvent::System { session_id: sid, subtype } => {
            match subtype.as_deref() {
                Some("init") => {
                    if let Some(sid) = sid {
                        tracing::info!("Got Claude session_id from init: {}", sid);
                        *agent_session_id = Some(sid);
                    }
                }
                Some("api_retry") => {
                    tracing::warn!("Claude API rate limited for session {:?}, aborting", sid);
                    return Err("API rate limited (429), please try again later".to_string());
                }
                other => {
                    tracing::info!("Claude system event: subtype={:?}, session={:?}", other, sid);
                }
            }
            Ok(None)
        }

        AgentEvent::Assistant { message, session_id: sid } => {
            if let Some(sid) = sid {
                if agent_session_id.is_none() {
                    *agent_session_id = Some(sid);
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
                            Some(make_tool_notification(session_id, &tool_call_id, &title, kind.as_ref()))
                        }
                        ContentBlock::ToolResultBlock { tool_use_id, content } => {
                            let tool_call_id = format!("tc_{}", tool_use_id);
                            let output_text = content.as_ref().and_then(|c| c.as_str());
                            Some(make_tool_update_notification(
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

        AgentEvent::ToolUse { id, name, input } => {
            let tool_call_id = format!("tc_{}", id);
            let title = format_tool_title(&name, &Some(input.clone()));
            let kind = infer_tool_kind(&name);
            Ok(Some(make_tool_notification(session_id, &tool_call_id, &title, kind.as_ref())))
        }

        AgentEvent::ToolResult { tool_use_id, content, is_error } => {
            let tool_call_id = format!("tc_{}", tool_use_id);
            let status = if is_error.unwrap_or(false) {
                ToolCallStatus::Failed
            } else {
                ToolCallStatus::Completed
            };
            let output_text = content.as_ref().and_then(|c| c.as_str());
            Ok(Some(make_tool_update_notification(session_id, &tool_call_id, status, output_text)))
        }

        AgentEvent::StreamEvent { inner } => {
            match inner.event_type.as_str() {
                "content_block_delta" => {
                    if let Some(delta) = inner.delta {
                        tracing::info!("[STREAM] content_block_delta: type={}, text_len={}", delta.delta_type, delta.text.len());
                        if delta.delta_type == "text_delta" && !delta.text.is_empty() {
                            accumulated_content.push_str(&delta.text);
                            return Ok(Some(make_chunk_notification(session_id, &delta.text)));
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

        AgentEvent::Result { result, stop_reason: reason, session_id: sid, is_error, .. } => {
            if let Some(sid) = sid {
                tracing::info!("Got Claude session_id from result: {}", sid);
                *agent_session_id = Some(sid);
            }

            if let Some(text) = result {
                tracing::info!("[STREAM] Result event, text_len={}, is_error={:?}, accumulated_empty={}", text.len(), is_error, accumulated_content.is_empty());
                if is_error.unwrap_or(false) {
                    return Err(format!("Claude error: {}", text));
                }
                if !text.is_empty() && accumulated_content.is_empty() {
                    accumulated_content.push_str(&text);
                    return Ok(Some(make_chunk_notification(session_id, &text)));
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

        AgentEvent::Error { error, .. } => {
            tracing::error!("Claude CLI error: {:?}", error);
            Ok(None)
        }
    }
}

