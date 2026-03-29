//! Streaming session support for ACP with Claude CLI JSON output parsing
//!
//! Uses Claude CLI with --output-format stream-json for real-time streaming.
//! CLI runs with --dangerously-skip-permissions, no permission handling needed.

use std::io::BufRead;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use uuid::Uuid;

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
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
        #[serde(rename = "_meta")]
        meta: Option<serde_json::Value>,
        #[serde(rename = "rawInput")]
        raw_input: Option<serde_json::Value>,
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

/// Tool call info for tracking and notifications
#[derive(Debug, Clone)]
pub struct StreamingToolCallInfo {
    pub tool_call_id: String,
    pub title: String,
    pub tool_name: Option<String>,
    pub raw_input: Option<serde_json::Value>,
}

/// Streaming result
#[derive(Debug)]
pub enum StreamingResult {
    /// Completed: (stop_reason, content, claude_session_id)
    Completed(StopReason, String, Option<String>),
    /// Error
    Error(String),
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

/// Streaming session state
pub struct StreamingSession {
    /// Session ID
    pub session_id: String,
    /// Agent info
    pub agent_info: AgentInfo,
    /// Working directory
    pub workdir: Option<String>,
    /// Session UUID (for maintaining context)
    session_uuid: String,
    /// Claude CLI session ID (for --resume)
    claude_session_id: std::cell::RefCell<Option<String>>,
    /// Pending tool calls (for tracking status)
    pending_tool_calls: std::collections::HashMap<String, StreamingToolCallInfo>,
}

impl StreamingSession {
    /// Create a new streaming session
    pub fn new(agent_info: AgentInfo, workdir: Option<&str>) -> Self {
        Self {
            session_id: format!("sess_{}", Uuid::new_v4().simple()),
            agent_info,
            workdir: workdir.map(|s| s.to_string()),
            session_uuid: Uuid::new_v4().to_string(),
            claude_session_id: std::cell::RefCell::new(None),
            pending_tool_calls: std::collections::HashMap::new(),
        }
    }

    /// Create from existing session (for ACP with session manager)
    pub fn from_session(
        session_id: String,
        session_uuid: String,
        agent_info: AgentInfo,
        workdir: Option<&str>,
        claude_session_id: Option<String>,
    ) -> Self {
        Self {
            session_id,
            agent_info,
            workdir: workdir.map(|s| s.to_string()),
            session_uuid,
            claude_session_id: std::cell::RefCell::new(claude_session_id),
            pending_tool_calls: std::collections::HashMap::new(),
        }
    }

    /// Get the current Claude session ID (for --resume)
    pub fn get_claude_session_id(&self) -> Option<String> {
        self.claude_session_id.borrow().clone()
    }

    /// Send prompt and stream results to callback
    ///
    /// The callback receives ACP sessionUpdate notifications as NDJSON strings
    pub fn send_prompt_streaming<F>(&mut self, message: &str, mut on_update: F) -> StreamingResult
    where
        F: FnMut(&str) -> Result<(), String>,
    {
        use crate::config::AgentType;

        match self.agent_info.agent_type {
            AgentType::Claude => self.run_claude(message, on_update),
            AgentType::Process => {
                match self.run_process(message, on_update) {
                    Ok((stop_reason, content)) => StreamingResult::Completed(stop_reason, content, None),
                    Err(e) => StreamingResult::Error(e),
                }
            }
            AgentType::Builtin => StreamingResult::Error("Builtin agents do not support streaming".to_string()),
        }
    }

    /// Run Claude CLI via PTY with JSON streaming
    fn run_claude<F>(&mut self, message: &str, mut on_update: F) -> StreamingResult
    where
        F: FnMut(&str) -> Result<(), String>,
    {
        // Build args: base args + session resume args + message
        let mut full_args: Vec<String> = self.agent_info.args.clone();

        let claude_sid = self.claude_session_id.borrow().clone();
        if let Some(ref sid) = claude_sid {
            for arg in &self.agent_info.session_args {
                full_args.push(arg.replace("${SESSION_ID}", sid));
            }
        }
        full_args.push(message.to_string());

        tracing::info!("Spawning with PTY: {} {:?}", self.agent_info.command, full_args);

        // Create PTY (line-buffered, real-time streaming)
        let pty_system = portable_pty::native_pty_system();
        let pair = match pty_system.openpty(portable_pty::PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        }) {
            Ok(p) => p,
            Err(e) => return StreamingResult::Error(format!("Failed to create PTY: {}", e)),
        };

        // Build command
        let mut cmd = portable_pty::cmdbuilder::CommandBuilder::new(&self.agent_info.command);
        cmd.args(full_args.iter().map(|s: &String| s.as_str()).collect::<Vec<&str>>());

        for (key, value) in &self.agent_info.env {
            cmd.env(key.clone(), value.clone());
        }
        for env_key in &self.agent_info.env_remove {
            cmd.env_remove(env_key.clone());
        }
        if let Some(ref dir) = self.workdir {
            cmd.cwd(dir.clone());
        }

        // Spawn via PTY
        let mut child = match pair.slave.spawn_command(cmd) {
            Ok(c) => c,
            Err(e) => return StreamingResult::Error(format!("Failed to spawn via PTY: {}", e)),
        };
        drop(pair.slave); // Drop slave so we get EOF when child exits

        let reader = match pair.master.try_clone_reader() {
            Ok(r) => r,
            Err(e) => return StreamingResult::Error(format!("Failed to get PTY reader: {}", e)),
        };
        let buf_reader = std::io::BufReader::new(reader);

        let mut stop_reason = StopReason::EndTurn;
        let mut accumulated_content = String::new();

        // Stream: PTY is line-buffered, we get each line immediately
        for line in buf_reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(e) => {
                    tracing::error!("Error reading line: {}", e);
                    continue;
                }
            };

            let line = strip_ansi_codes(&line);
            if line.trim().is_empty() {
                continue;
            }

            tracing::debug!("PTY line ({} bytes): {}", line.len(), &line[..line.len().min(200)]);

            match self.process_event(&line, &mut on_update, &mut accumulated_content) {
                Ok(Some(reason)) => stop_reason = reason,
                Ok(None) => {}
                Err(e) => tracing::warn!("Error processing event: {}", e),
            }
        }

        // Wait for process to complete
        match child.wait() {
            Ok(status) => {
                tracing::info!("Process completed with status: {:?}", status);
                if !status.success() {
                    tracing::warn!("Claude process exited with status: {:?}", status);
                }
            }
            Err(e) => {
                tracing::error!("Wait error: {}", e);
                return StreamingResult::Error(format!("Wait error: {}", e));
            }
        }

        tracing::info!("Streaming completed, content length: {}", accumulated_content.len());
        let claude_sid = self.claude_session_id.borrow().clone();
        StreamingResult::Completed(stop_reason, accumulated_content, claude_sid)
    }

    /// Process a single Claude JSON event line
    fn process_event<F>(
        &mut self,
        line: &str,
        on_update: &mut F,
        accumulated_content: &mut String,
    ) -> Result<Option<StopReason>, String>
    where
        F: FnMut(&str) -> Result<(), String>,
    {
        let event: ClaudeEvent = match serde_json::from_str(line) {
            Ok(e) => e,
            Err(e) => {
                tracing::debug!("Not a Claude JSON event: {}", e);
                return Ok(None);
            }
        };

        match event {
            ClaudeEvent::System { session_id } => {
                if let Some(sid) = session_id {
                    tracing::info!("Got Claude session_id from init: {}", sid);
                    *self.claude_session_id.borrow_mut() = Some(sid);
                }
            }

            ClaudeEvent::Assistant { message, session_id } => {
                if let Some(sid) = session_id {
                    if self.claude_session_id.borrow().is_none() {
                        *self.claude_session_id.borrow_mut() = Some(sid);
                    }
                }
                if let Some(msg) = message {
                    for block in msg.content {
                        match block {
                            ContentBlock::Text { text } => {
                                accumulated_content.push_str(&text);
                                on_update(&self.make_message_chunk(&text))?;
                            }
                            ContentBlock::Thinking { thinking } => {
                                tracing::debug!("Thinking: {} chars", thinking.len());
                            }
                            ContentBlock::ToolUse { id, name, input } => {
                                let tool_call_id = format!("tc_{}", id);
                                let title = self.format_tool_title(&name, &Some(input.clone()));
                                let tool_call = StreamingToolCallInfo {
                                    tool_call_id: tool_call_id.clone(),
                                    title: title.clone(),
                                    tool_name: Some(name.clone()),
                                    raw_input: Some(input.clone()),
                                };
                                self.pending_tool_calls.insert(id, tool_call.clone());
                                let (kind, _) = self.parse_tool_info(&tool_call);
                                on_update(&self.make_tool_call(&tool_call_id, &title, &kind))?;
                            }
                            ContentBlock::ToolResultBlock { tool_use_id, content } => {
                                let tool_call_id = format!("tc_{}", tool_use_id);
                                let output_text = content.as_ref().and_then(|c| c.as_str());
                                on_update(&self.make_tool_call_update(&tool_call_id, ToolCallStatus::Completed, output_text))?;
                                self.pending_tool_calls.remove(&tool_use_id);
                            }
                        }
                    }
                }
            }

            ClaudeEvent::ToolUse { id, name, input, meta, raw_input } => {
                let tool_call_id = format!("tc_{}", id);
                let title = self.format_tool_title(&name, &Some(input.clone()));
                let tool_call = StreamingToolCallInfo {
                    tool_call_id: tool_call_id.clone(),
                    title: title.clone(),
                    tool_name: self.resolve_tool_name(&meta, &raw_input, &name),
                    raw_input: raw_input.clone().or(Some(input.clone())),
                };
                self.pending_tool_calls.insert(id, tool_call.clone());
                let (kind, _) = self.parse_tool_info(&tool_call);
                on_update(&self.make_tool_call(&tool_call_id, &title, &kind))?;
            }

            ClaudeEvent::ToolResult { tool_use_id, content, is_error } => {
                let tool_call_id = format!("tc_{}", tool_use_id);
                let status = if is_error.unwrap_or(false) {
                    ToolCallStatus::Failed
                } else {
                    ToolCallStatus::Completed
                };
                let output_text = content.as_ref().and_then(|c| c.as_str());
                on_update(&self.make_tool_call_update(&tool_call_id, status, output_text))?;
                self.pending_tool_calls.remove(&tool_use_id);
            }

            ClaudeEvent::Result { result, stop_reason, session_id, is_error } => {
                if let Some(sid) = session_id {
                    tracing::info!("Got Claude session_id from result: {}", sid);
                    *self.claude_session_id.borrow_mut() = Some(sid);
                }

                if let Some(text) = result {
                    if !text.is_empty() && accumulated_content.is_empty() {
                        accumulated_content.push_str(&text);
                        on_update(&self.make_message_chunk(&text))?;
                    }
                    if is_error.unwrap_or(false) {
                        return Err(format!("Claude error: {}", text));
                    }
                }

                let reason = match stop_reason.as_deref() {
                    Some("end_turn") => StopReason::EndTurn,
                    Some("stop") => StopReason::Stop,
                    Some("tool_use") => StopReason::EndTurn,
                    _ => StopReason::EndTurn,
                };
                return Ok(Some(reason));
            }
        }

        Ok(None)
    }

    /// Resolve tool name from metadata
    fn resolve_tool_name(
        &self,
        meta: &Option<serde_json::Value>,
        raw_input: &Option<serde_json::Value>,
        default: &str,
    ) -> Option<String> {
        if let Some(meta) = meta {
            for key in &["toolName", "tool_name", "name"] {
                if let Some(name) = meta.get(key).and_then(|v| v.as_str()) {
                    return Some(name.to_string());
                }
            }
        }
        if let Some(raw) = raw_input {
            for key in &["tool", "toolName", "tool_name", "name"] {
                if let Some(name) = raw.get(key).and_then(|v| v.as_str()) {
                    return Some(name.to_string());
                }
            }
        }
        Some(default.to_string())
    }

    /// Format tool title for display
    fn format_tool_title(&self, name: &str, input: &Option<serde_json::Value>) -> String {
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
                let truncated = if cmd.len() > 40 {
                    format!("{}...", &cmd[..37])
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

    /// Parse tool info for notifications
    fn parse_tool_info(&self, tool_call: &StreamingToolCallInfo) -> (Option<ToolKind>, String) {
        let kind = match tool_call.tool_name.as_deref() {
            Some("Read" | "NBRead") => Some(ToolKind::Read),
            Some("Edit" | "NBEdit") => Some(ToolKind::Edit),
            Some("Write" | "NBWrite") => Some(ToolKind::Other),
            Some("Delete") => Some(ToolKind::Delete),
            Some("Glob" | "Grep") => Some(ToolKind::Search),
            Some("Bash") => Some(ToolKind::Execute),
            Some("WebFetch") => Some(ToolKind::Fetch),
            _ => Some(ToolKind::Other),
        };
        (kind, tool_call.title.clone())
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

    /// Run external process (non-streaming, collects all output)
    fn run_process<F>(&mut self, message: &str, mut on_update: F) -> Result<(StopReason, String), String>
    where
        F: FnMut(&str) -> Result<(), String>,
    {
        tracing::info!("run_process: agent_id={}, command={}, args={:?}",
            self.agent_info.id, self.agent_info.command, self.agent_info.args);

        if self.agent_info.command.is_empty() {
            return Err(format!("Agent {} has no command configured", self.agent_info.id));
        }

        let mut cmd = std::process::Command::new(&self.agent_info.command);
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
            let result = String::from_utf8_lossy(&output.stdout).to_string();
            if !result.is_empty() {
                on_update(&self.make_message_chunk(&result))?;
            }
            Ok((StopReason::EndTurn, result))
        } else {
            let error = String::from_utf8_lossy(&output.stderr).to_string();
            Err(format!("Process error: {}", error))
        }
    }

    /// Run Claude CLI asynchronously with tokio::process::Command
    ///
    /// Returns a receiver that yields streaming events in real-time.
    /// Each line from stdout is parsed and sent immediately as a notification.
    pub async fn run_claude_async(
        session_id: String,
        agent_info: AgentInfo,
        workdir: Option<String>,
        claude_session_id: Option<String>,
        message: &str,
    ) -> Result<mpsc::Receiver<StreamingEvent>, String> {
        use std::process::Stdio;

        // Build args: base args + session resume args + message
        let mut full_args: Vec<String> = agent_info.args.clone();

        if let Some(ref sid) = claude_session_id {
            for arg in &agent_info.session_args {
                full_args.push(arg.replace("${SESSION_ID}", sid));
            }
        }
        full_args.push(message.to_string());

        tracing::info!("Spawning async: {} {:?}", agent_info.command, full_args);

        // Create channel for streaming events
        let (tx, rx) = mpsc::channel::<StreamingEvent>(64);

        // Build async command
        let mut cmd = Command::new(&agent_info.command);
        cmd.args(&full_args);

        // Set environment
        for (key, value) in &agent_info.env {
            cmd.env(key, value);
        }
        for env_key in &agent_info.env_remove {
            cmd.env_remove(env_key);
        }

        // Set working directory
        if let Some(ref dir) = workdir {
            cmd.current_dir(dir);
        }

        // Configure stdio for async reading
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        // Spawn process
        let mut child = cmd.spawn()
            .map_err(|e| format!("Failed to spawn {}: {}", agent_info.command, e))?;

        let stdout = child.stdout.take()
            .ok_or("Failed to capture stdout")?;
        let stderr = child.stderr.take()
            .ok_or("Failed to capture stderr")?;

        // Clone data for async task
        let session_id_clone = session_id.clone();
        let tx_clone = tx.clone();

        // Spawn task to process stdout
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

                tracing::debug!("Async line: {}", &line[..line.len().min(200)]);

                // Process the line
                match Self::process_line_static(
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

            // Send completion event
            let result = AsyncStreamingResult::Completed {
                stop_reason,
                content: accumulated_content,
                claude_session_id,
            };

            let _ = tx_clone.send(StreamingEvent::Completed(result)).await;
        });

        // Spawn task to handle stderr (logging only)
        let tx_err = tx.clone();
        let stderr_task = tokio::spawn(async move {
            let reader = BufReader::new(stderr);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::warn!("Claude stderr: {}", line);
            }
        });

        // Spawn task to wait for process completion
        tokio::spawn(async move {
            let _ = stdout_task.await;
            let _ = stderr_task.await;
            match child.wait().await {
                Ok(status) => {
                    tracing::info!("Process completed: {:?}", status);
                }
                Err(e) => {
                    tracing::error!("Wait error: {}", e);
                }
            }
        });

        Ok(rx)
    }

    /// Process a single line and return notification if any
    fn process_line_static(
        session_id: &str,
        line: &str,
        accumulated_content: &mut String,
        claude_session_id: &mut Option<String>,
        stop_reason: &mut StopReason,
    ) -> Result<Option<String>, String> {
        let event: ClaudeEvent = match serde_json::from_str(line) {
            Ok(e) => e,
            Err(e) => {
                tracing::debug!("Not a Claude JSON event: {}", e);
                return Ok(None);
            }
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
                if let Some(msg) = message {
                    for block in msg.content {
                        match block {
                            ContentBlock::Text { text } => {
                                accumulated_content.push_str(&text);
                                return Ok(Some(Self::make_message_chunk_static(session_id, &text)));
                            }
                            ContentBlock::Thinking { thinking } => {
                                tracing::debug!("Thinking: {} chars", thinking.len());
                            }
                            ContentBlock::ToolUse { id, name, input } => {
                                let tool_call_id = format!("tc_{}", id);
                                let title = Self::format_tool_title_static(&name, &Some(input.clone()));
                                let kind = Self::infer_tool_kind(&name);
                                return Ok(Some(Self::make_tool_call_static(session_id, &tool_call_id, &title, &kind)));
                            }
                            ContentBlock::ToolResultBlock { tool_use_id, content } => {
                                let tool_call_id = format!("tc_{}", tool_use_id);
                                let output_text = content.as_ref().and_then(|c| c.as_str());
                                return Ok(Some(Self::make_tool_call_update_static(
                                    session_id,
                                    &tool_call_id,
                                    ToolCallStatus::Completed,
                                    output_text,
                                )));
                            }
                        }
                    }
                }
                Ok(None)
            }

            ClaudeEvent::ToolUse { id, name, input, meta, raw_input } => {
                let tool_call_id = format!("tc_{}", id);
                let title = Self::format_tool_title_static(&name, &Some(input.clone()));
                let kind = Self::infer_tool_kind(&name);
                Ok(Some(Self::make_tool_call_static(session_id, &tool_call_id, &title, &kind)))
            }

            ClaudeEvent::ToolResult { tool_use_id, content, is_error } => {
                let tool_call_id = format!("tc_{}", tool_use_id);
                let status = if is_error.unwrap_or(false) {
                    ToolCallStatus::Failed
                } else {
                    ToolCallStatus::Completed
                };
                let output_text = content.as_ref().and_then(|c| c.as_str());
                Ok(Some(Self::make_tool_call_update_static(session_id, &tool_call_id, status, output_text)))
            }

            ClaudeEvent::Result { result, stop_reason: reason, session_id: sid, is_error } => {
                if let Some(sid) = sid {
                    tracing::info!("Got Claude session_id from result: {}", sid);
                    *claude_session_id = Some(sid);
                }

                if let Some(text) = result {
                    if !text.is_empty() && accumulated_content.is_empty() {
                        accumulated_content.push_str(&text);
                        // Send the final content as notification
                        let _ = Self::make_message_chunk_static(session_id, &text);
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

    /// Static helper: create message chunk notification
    fn make_message_chunk_static(session_id: &str, text: &str) -> String {
        let notification = SessionUpdateParams {
            sessionId: session_id.to_string(),
            update: SessionUpdate::AgentMessageChunk {
                content: MessageContent::Text { text: text.to_string() },
            },
        };
        serde_json::to_string(&AcpResponse::notification("sessionUpdate", notification))
            .unwrap_or_else(|_| "{}".to_string())
    }

    /// Static helper: create tool_call notification
    fn make_tool_call_static(session_id: &str, tool_call_id: &str, title: &str, kind: &Option<ToolKind>) -> String {
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

    /// Static helper: create tool_call_update notification
    fn make_tool_call_update_static(session_id: &str, tool_call_id: &str, status: ToolCallStatus, output: Option<&str>) -> String {
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

    /// Static helper: format tool title
    fn format_tool_title_static(name: &str, input: &Option<serde_json::Value>) -> String {
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
                let truncated = if cmd.len() > 40 {
                    format!("{}...", &cmd[..37])
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

    /// Static helper: infer tool kind from name
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
}

/// Strip ANSI escape codes from PTY output
fn strip_ansi_codes(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut result = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            i += 1;
            if i >= bytes.len() {
                break;
            }
            if bytes[i] == b'[' {
                // CSI: \x1b[ ... final_byte (0x40-0x7E)
                i += 1;
                while i < bytes.len() && bytes[i] < 0x40 {
                    i += 1;
                }
                if i < bytes.len() {
                    i += 1;
                }
            } else if bytes[i] == b']' {
                // OSC: \x1b] ... \x07 or \x1b\\
                i += 1;
                while i < bytes.len() && bytes[i] != 0x07 && bytes[i] != 0x1b {
                    i += 1;
                }
                if i < bytes.len() && bytes[i] == 0x07 {
                    i += 1;
                } else if i + 1 < bytes.len() && bytes[i] == 0x1b && bytes[i + 1] == b'\\' {
                    i += 2;
                }
            } else {
                i += 1;
            }
        } else if bytes[i] == b'\r' {
            i += 1;
        } else {
            result.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&result).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_agent_info() -> AgentInfo {
        AgentInfo {
            id: "test".to_string(),
            name: "Test".to_string(),
            capabilities: vec![],
            agent_type: crate::config::AgentType::Claude,
            command: String::new(),
            args: vec![],
            help_command: String::new(),
            working_dir: None,
            require_workdir: false,
            env: std::collections::HashMap::new(),
            env_remove: vec![],
            session_args: vec![],
            default_allowed_tools: vec![],
        }
    }

    #[test]
    fn test_resolve_tool_name() {
        let session = StreamingSession::new(test_agent_info(), None);

        // From meta
        let meta = serde_json::json!({"toolName": "write"});
        assert_eq!(
            session.resolve_tool_name(&Some(meta), &None, "default"),
            Some("write".to_string())
        );

        // From raw input
        let raw = serde_json::json!({"tool": "read"});
        assert_eq!(
            session.resolve_tool_name(&None, &Some(raw), "default"),
            Some("read".to_string())
        );

        // Default fallback
        assert_eq!(
            session.resolve_tool_name(&None, &None, "default"),
            Some("default".to_string())
        );
    }

    #[test]
    fn test_strip_ansi() {
        // CSI color code
        assert_eq!(strip_ansi_codes("\x1b[32mhello\x1b[0m"), "hello");
        // Bold
        assert_eq!(strip_ansi_codes("\x1b[1;34mtext\x1b[0m"), "text");
        // Carriage return
        assert_eq!(strip_ansi_codes("foo\r\nbar"), "foo\nbar");
        // Plain text unchanged
        assert_eq!(strip_ansi_codes("plain text"), "plain text");
    }
}
