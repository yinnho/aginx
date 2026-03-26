//! Streaming session support for ACP with Claude CLI JSON output parsing
//!
//! Uses Claude CLI with --output-format json for structured output
//! Detects permission requests from tool_use events

use std::io::BufRead;
use std::sync::mpsc::{Receiver, Sender};

use uuid::Uuid;

use super::types::{
    AcpResponse, PermissionOption as AcpPermissionOption, RequestPermissionParams, SessionUpdate, SessionUpdateParams,
    MessageContent, ToolCallStatus, ToolCallContent, ToolKind, StopReason, ToolCallInfo,
};
use crate::agent::{AgentInfo, PermissionOption};

/// Claude CLI JSON output event
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(tag = "type")]
enum ClaudeEvent {
    #[serde(rename = "text")]
    Text { text: String },
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
    #[serde(rename = "stop")]
    Stop {
        #[serde(rename = "stop_reason")]
        stop_reason: String,
    },
}

/// Permission request from streaming session
#[derive(Debug, Clone)]
pub struct StreamingPermissionRequest {
    /// Request ID
    pub request_id: String,
    /// Description
    pub description: String,
    /// Tool call info (structured, not parsed from text)
    pub tool_call: Option<StreamingToolCallInfo>,
    /// Options
    pub options: Vec<PermissionOption>,
}

/// Tool call information for permission requests (internal)
#[derive(Debug, Clone)]
pub struct StreamingToolCallInfo {
    pub tool_call_id: String,
    pub title: String,
    pub tool_name: Option<String>,
    pub raw_input: Option<serde_json::Value>,
}

/// Streaming result - either continue with notifications or pause for permission
#[derive(Debug)]
pub enum StreamingResult {
    /// Completed successfully with accumulated response content
    Completed(StopReason, String),
    /// Need permission before continuing
    PermissionNeeded {
        request: StreamingPermissionRequest,
        /// Channel to receive permission response (choice index)
        response_tx: Sender<usize>,
        /// Channel to wait for permission response
        response_rx: Receiver<usize>,
    },
    /// Error occurred
    Error(String),
}

/// Action to take after processing a line
#[derive(Debug)]
pub enum StreamingAction {
    /// Stop with reason
    Stop(StopReason),
    /// Permission needed before continuing
    PermissionNeeded(StreamingPermissionRequest),
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
    /// Tool call counter (for generating tool call IDs)
    tool_call_counter: u32,
    /// Pending tool calls (for tracking permission state)
    pending_tool_calls: std::collections::HashMap<String, StreamingToolCallInfo>,
}

impl StreamingSession {
    /// Create a new streaming session (for standalone use)
    pub fn new(agent_info: AgentInfo, workdir: Option<&str>) -> Self {
        let session_id = format!("sess_{}", Uuid::new_v4().simple());
        let session_uuid = Uuid::new_v4().to_string();

        Self {
            session_id,
            agent_info,
            workdir: workdir.map(|s| s.to_string()),
            session_uuid,
            tool_call_counter: 0,
            pending_tool_calls: std::collections::HashMap::new(),
        }
    }

    /// Create from existing session (for ACP with session manager)
    pub fn from_session(
        session_id: String,
        session_uuid: String,
        agent_info: AgentInfo,
        workdir: Option<&str>,
    ) -> Self {
        Self {
            session_id,
            agent_info,
            workdir: workdir.map(|s| s.to_string()),
            session_uuid,
            tool_call_counter: 0,
            pending_tool_calls: std::collections::HashMap::new(),
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
            AgentType::Claude => self.send_prompt_json(message, on_update),
            AgentType::Process => self.send_prompt_process_streaming(message, on_update),
            AgentType::Builtin => Err("Builtin agents do not support streaming".to_string()),
        }
    }

    /// Send prompt using configured command and session_args
    fn send_prompt_json<F>(&mut self, message: &str, mut on_update: F) -> Result<StopReason, String>
    where
        F: FnMut(&str) -> Result<(), String>,
    {
        // Build command using configured command
        let mut cmd = std::process::Command::new(&self.agent_info.command);

        // Add configured args
        for arg in &self.agent_info.args {
            cmd.arg(arg);
        }

        // Add session args (支持 ${SESSION_ID} 变量替换)
        for arg in &self.agent_info.session_args {
            let processed_arg = arg.replace("${SESSION_ID}", &self.session_uuid);
            cmd.arg(processed_arg);
        }

        // Remove configured environment variables
        for env_key in &self.agent_info.env_remove {
            cmd.env_remove(env_key);
        }

        // Add configured environment variables
        for (key, value) in &self.agent_info.env {
            cmd.env(key, value);
        }

        if let Some(ref dir) = self.workdir {
            cmd.current_dir(dir);
        }

        // Add the message as argument
        cmd.arg(message);

        // Spawn the process
        let mut child = cmd
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to spawn claude: {}", e))?;

        // Read stdout
        let stdout = child.stdout.take().ok_or("Failed to get stdout")?;
        let buf_reader = std::io::BufReader::new(stdout);

        let mut stop_reason = StopReason::EndTurn;
        let mut accumulated_content = String::new();

        // Process Claude's JSON stream
        for line in buf_reader.lines() {
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

            tracing::debug!("Claude JSON line: {}", line);

            // Parse Claude event
            match self.process_claude_event(&line, &mut on_update, &mut accumulated_content) {
                Ok(Some(StreamingAction::Stop(reason))) => {
                    stop_reason = reason;
                    // Don't break immediately, continue reading to get all output
                }
                Ok(Some(StreamingAction::PermissionNeeded(perm))) => {
                    // Send permission request notification
                    let perm_notification = self.make_permission_request_notification(&perm);
                    on_update(&perm_notification)?;

                    // In this simplified version, we auto-allow for now
                    // In production, this would pause and wait for user response
                    tracing::info!("Permission needed for {} - auto-allowing for now", perm.description);
                }
                Ok(None) => continue,
                Err(e) => {
                    tracing::error!("Error processing Claude event: {}", e);
                }
            }
        }

        // Wait for process to complete
        let output = child.wait_with_output()
            .map_err(|e| format!("Failed to wait for process: {}", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.is_empty() {
                return Err(format!("Claude process error: {}", stderr));
            }
        }

        Ok(stop_reason)
    }

    /// Send prompt with interactive mode and external permission channel
    /// This version accepts a permission receiver so the handler can signal responses
    pub fn send_prompt_interactive_with_permission<F>(
        &mut self,
        message: &str,
        mut on_update: F,
        perm_rx: std::sync::mpsc::Receiver<usize>,
    ) -> StreamingResult
    where
        F: FnMut(&str) -> Result<(), String>,
    {
        use crate::config::AgentType;

        match self.agent_info.agent_type {
            AgentType::Claude => {
                // Use JSON-based implementation with permission handling
                self.send_prompt_json_with_permission(message, on_update, perm_rx)
            }
            AgentType::Process => {
                match self.send_prompt_process_streaming(message, on_update) {
                    Ok(stop_reason) => StreamingResult::Completed(stop_reason, String::new()),
                    Err(e) => StreamingResult::Error(e),
                }
            }
            AgentType::Builtin => StreamingResult::Error("Builtin agents do not support streaming".to_string()),
        }
    }

    /// Send prompt using configured command and session_args with external permission channel
    fn send_prompt_json_with_permission<F>(
        &mut self,
        message: &str,
        mut on_update: F,
        perm_rx: std::sync::mpsc::Receiver<usize>,
    ) -> StreamingResult
    where
        F: FnMut(&str) -> Result<(), String>,
    {
        // Build command using configured command
        let mut cmd = std::process::Command::new(&self.agent_info.command);

        // Add configured args
        for arg in &self.agent_info.args {
            cmd.arg(arg);
        }

        // Add session args (支持 ${SESSION_ID} 变量替换)
        for arg in &self.agent_info.session_args {
            let processed_arg = arg.replace("${SESSION_ID}", &self.session_uuid);
            cmd.arg(processed_arg);
        }

        // Remove configured environment variables
        for env_key in &self.agent_info.env_remove {
            cmd.env_remove(env_key);
        }

        // Add configured environment variables
        for (key, value) in &self.agent_info.env {
            cmd.env(key, value);
        }

        if let Some(ref dir) = self.workdir {
            cmd.current_dir(dir);
        }

        // Add the message as argument
        cmd.arg(message);

        // Spawn the process
        let mut child = match cmd
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => return StreamingResult::Error(format!("Failed to spawn claude: {}", e)),
        };

        // Read stdout
        let stdout = match child.stdout.take() {
            Some(s) => s,
            None => return StreamingResult::Error("Failed to get stdout".to_string()),
        };
        let buf_reader = std::io::BufReader::new(stdout);

        // Read stderr in a separate thread to avoid deadlock
        let stderr = child.stderr.take();
        let stderr_handle = std::thread::spawn(move || {
            let mut stderr_content = String::new();
            if let Some(stderr) = stderr {
                use std::io::BufRead;
                let reader = std::io::BufReader::new(stderr);
                for line in reader.lines() {
                    if let Ok(l) = line {
                        stderr_content.push_str(&l);
                        stderr_content.push('\n');
                    }
                }
            }
            stderr_content
        });

        let mut stop_reason = StopReason::EndTurn;
        let mut accumulated_content = String::new();

        // Collect all output (for --output-format json, it's a single JSON object)
        let mut all_output = String::new();
        for line in buf_reader.lines() {
            match line {
                Ok(l) => {
                    all_output.push_str(&l);
                    all_output.push('\n');
                }
                Err(e) => {
                    tracing::error!("Error reading line: {}", e);
                }
            }
        }

        // Try to parse as single JSON result (--output-format json format)
        tracing::debug!("Parsing output as JSON, length: {}", all_output.len());
        if let Ok(json_result) = serde_json::from_str::<serde_json::Value>(&all_output) {
            // Check if it's a result object with "result" field
            if let Some(result_text) = json_result.get("result").and_then(|r| r.as_str()) {
                tracing::info!("Got result from JSON: {} chars", result_text.len());
                accumulated_content = result_text.to_string();
                // Send as a single message chunk
                let notification = self.make_message_chunk(result_text);
                tracing::debug!("Sending notification...");
                if let Err(e) = on_update(&notification) {
                    tracing::error!("Failed to send notification: {}", e);
                    return StreamingResult::Error(format!("Failed to send notification: {}", e));
                }
                tracing::debug!("Notification sent successfully");
                // Extract stop_reason if present
                if let Some(sr) = json_result.get("stop_reason").and_then(|r| r.as_str()) {
                    stop_reason = match sr {
                        "end_turn" => StopReason::EndTurn,
                        "stop" => StopReason::Stop,
                        _ => StopReason::EndTurn,
                    };
                }
                tracing::info!("JSON result processed, stop_reason: {:?}", stop_reason);
            } else {
                // Try parsing each line as a streaming event
                for line in all_output.lines() {
                    if line.trim().is_empty() {
                        continue;
                    }
                    tracing::debug!("Claude JSON line: {}", line);

                    // Parse Claude event
                    match self.process_claude_event(line, &mut on_update, &mut accumulated_content) {
                        Ok(Some(StreamingAction::Stop(reason))) => {
                            stop_reason = reason;
                        }
                        Ok(Some(StreamingAction::PermissionNeeded(perm))) => {
                            // Send permission request notification
                            let perm_notification = self.make_permission_request_notification(&perm);
                            if let Err(e) = on_update(&perm_notification) {
                                return StreamingResult::Error(format!("Failed to send notification: {}", e));
                            }

                            // Wait for permission response from the channel
                            match perm_rx.recv() {
                                Ok(_choice) => {
                                    // For now, we just continue
                                    // In a full implementation, we would restart Claude with the permission granted
                                    tracing::info!("Permission response received, continuing...");
                                }
                                Err(_) => {
                                    return StreamingResult::Error("Permission channel closed".to_string());
                                }
                            }
                        }
                        Ok(None) => continue,
                        Err(e) => {
                            tracing::error!("Error processing Claude event: {}", e);
                        }
                    }
                }
            }
        }

        // Wait for process to complete
        tracing::debug!("Waiting for process to complete...");
        match child.wait() {
            Ok(status) => {
                tracing::info!("Process completed with status: {:?}", status);
                if !status.success() {
                    // Get stderr content
                    let stderr_content = stderr_handle.join().unwrap_or_default();
                    tracing::warn!("Claude process exited with status: {:?}, stderr: {}", status, stderr_content);
                }
            }
            Err(e) => {
                tracing::error!("Wait error: {}", e);
                return StreamingResult::Error(format!("Wait error: {}", e));
            }
        }

        tracing::info!("Streaming completed, content length: {}", accumulated_content.len());
        StreamingResult::Completed(stop_reason, accumulated_content)
    }

    /// Process a single Claude JSON event
    fn process_claude_event<F>(&mut self, line: &str, on_update: &mut F, accumulated_content: &mut String) -> Result<Option<StreamingAction>, String>
    where
        F: FnMut(&str) -> Result<(), String>,
    {
        // Parse as ClaudeEvent
        let event: ClaudeEvent = match serde_json::from_str(line) {
            Ok(e) => e,
            Err(e) => {
                // Not a Claude event, might be regular text
                tracing::debug!("Not a Claude JSON event: {}", e);
                return Ok(None);
            }
        };

        match event {
            ClaudeEvent::Text { text } => {
                // Accumulate content for final response
                accumulated_content.push_str(&text);
                // Send as message chunk
                let notification = self.make_message_chunk(&text);
                on_update(&notification)?;
            }
            ClaudeEvent::ToolUse { id, name, input, meta, raw_input } => {
                // Create tool call info
                let tool_call_id = format!("tc_{}", id);
                let title = self.format_tool_title(&name, &Some(input.clone()));

                let tool_call = StreamingToolCallInfo {
                    tool_call_id: tool_call_id.clone(),
                    title: title.clone(),
                    tool_name: self.resolve_tool_name(&meta, &raw_input, &name),
                    raw_input: raw_input.clone().or(Some(input.clone())),
                };

                // Store for later reference
                self.pending_tool_calls.insert(id.clone(), tool_call.clone());

                // Send tool_call notification
                let (kind, _) = self.parse_tool_info(&tool_call);
                let notification = self.make_tool_call(&tool_call_id, &title, &kind);
                on_update(&notification)?;

                // Check if permission is needed
                if self.tool_requires_permission(&tool_call) {
                    // Return permission needed action
                    let options = vec![
                        PermissionOption {
                            index: 1,
                            label: "Allow this time".to_string(),
                            is_default: true,
                        },
                        PermissionOption {
                            index: 2,
                            label: "Always allow".to_string(),
                            is_default: false,
                        },
                        PermissionOption {
                            index: 3,
                            label: "Deny this time".to_string(),
                            is_default: false,
                        },
                        PermissionOption {
                            index: 4,
                            label: "Always deny".to_string(),
                            is_default: false,
                        },
                    ];

                    return Ok(Some(StreamingAction::PermissionNeeded(StreamingPermissionRequest {
                        request_id: format!("perm_{}", Uuid::new_v4().simple()),
                        description: format!("Allow {}?", title),
                        tool_call: Some(tool_call),
                        options,
                    })));
                }
            }
            ClaudeEvent::ToolResult { tool_use_id, content, is_error } => {
                let tool_call_id = format!("tc_{}", tool_use_id);
                let status = if is_error.unwrap_or(false) {
                    ToolCallStatus::Failed
                } else {
                    ToolCallStatus::Completed
                };

                let output_text = content.as_ref().and_then(|c| c.as_str());
                let notification = self.make_tool_call_update(&tool_call_id, status, output_text);
                on_update(&notification)?;

                // Remove from pending
                self.pending_tool_calls.remove(&tool_use_id);
            }
            ClaudeEvent::Stop { stop_reason } => {
                let reason = match stop_reason.as_str() {
                    "end_turn" => StopReason::EndTurn,
                    "stop" => StopReason::Stop,
                    "tool_use" => StopReason::EndTurn,  // Treat as end_turn for now
                    _ => StopReason::EndTurn,
                };
                return Ok(Some(StreamingAction::Stop(reason)));
            }
        }

        Ok(None)
    }

    /// Resolve tool name from metadata (openclaw pattern)
    fn resolve_tool_name(
        &self,
        meta: &Option<serde_json::Value>,
        raw_input: &Option<serde_json::Value>,
        default: &str,
    ) -> Option<String> {
        // 1. From _meta.toolName / _meta.tool_name / _meta.name
        if let Some(meta) = meta {
            for key in &["toolName", "tool_name", "name"] {
                if let Some(name) = meta.get(key).and_then(|v| v.as_str()) {
                    return Some(name.to_string());
                }
            }
        }

        // 2. From rawInput.tool / rawInput.toolName / rawInput.tool_name
        if let Some(raw) = raw_input {
            for key in &["tool", "toolName", "tool_name", "name"] {
                if let Some(name) = raw.get(key).and_then(|v| v.as_str()) {
                    return Some(name.to_string());
                }
            }
        }

        // 3. From default (the tool name from Claude)
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

    /// Check if tool requires permission (based on tool type, not regex)
    fn tool_requires_permission(&self, tool_call: &StreamingToolCallInfo) -> bool {
        // All tools require permission in strict mode
        // This can be refined based on tool_call.tool_name
        match tool_call.tool_name.as_deref() {
            Some("read" | "Read" | "NBRead") => false, // Read is generally safe
            Some("glob" | "Glob") => false,             // Glob is generally safe
            Some("grep" | "Grep") => false,             // Grep is generally safe
            Some("edit" | "Edit" | "NBEdit") => true,
            Some("write" | "Write" | "NBWrite") => true,
            Some("delete" | "Delete") => true,
            Some("bash" | "Bash") => true,
            _ => true, // Default to requiring permission
        }
    }

    /// Make permission request notification
    fn make_permission_request_notification(&self, perm: &StreamingPermissionRequest) -> String {
        let options: Vec<AcpPermissionOption> = perm.options.iter().enumerate().map(|(i, opt)| {
            AcpPermissionOption {
                optionId: (i + 1).to_string(),
                label: opt.label.clone(),
                kind: Some(match i {
                    0 => "allow_once".to_string(),
                    1 => "allow_always".to_string(),
                    2 => "reject_once".to_string(),
                    _ => "reject_always".to_string(),
                }),
            }
        }).collect();

        let tool_call_info = perm.tool_call.as_ref().map(|tc| ToolCallInfo {
            toolCallId: tc.tool_call_id.clone(),
            title: Some(tc.title.clone()),
            _meta: tc.tool_name.as_ref().map(|n| serde_json::json!({"toolName": n})),
        });

        let params = RequestPermissionParams {
            requestId: perm.request_id.clone(),
            description: Some(perm.description.clone()),
            toolCall: tool_call_info,
            options,
        };

        serde_json::to_string(&AcpResponse::notification("requestPermission", params))
            .unwrap_or_else(|_| "{}".to_string())
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

    /// Send prompt to external process (non-streaming for now)
    fn send_prompt_process_streaming<F>(&mut self, message: &str, mut on_update: F) -> Result<StopReason, String>
    where
        F: FnMut(&str) -> Result<(), String>,
    {
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

    /// Send prompt with interactive mode (wrapper around JSON implementation)
    /// Returns StreamingResult to support permission handling
    pub fn send_prompt_interactive<F>(&mut self, message: &str, mut on_update: F) -> StreamingResult
    where
        F: FnMut(&str) -> Result<(), String>,
    {
        use crate::config::AgentType;

        match self.agent_info.agent_type {
            AgentType::Claude => {
                // For now, just complete without permission handling
                // In production, this would use the permission channel
                match self.send_prompt_json(message, on_update) {
                    Ok(stop_reason) => StreamingResult::Completed(stop_reason, String::new()),
                    Err(e) => StreamingResult::Error(e),
                }
            }
            AgentType::Process => {
                match self.send_prompt_process_streaming(message, on_update) {
                    Ok(stop_reason) => StreamingResult::Completed(stop_reason, String::new()),
                    Err(e) => StreamingResult::Error(e),
                }
            }
            AgentType::Builtin => StreamingResult::Error("Builtin agents do not support streaming".to_string()),
        }
    }

    /// Send permission response - no-op for JSON mode since we restart Claude
    pub fn send_permission_response(&mut self, _choice: usize) -> Result<(), String> {
        // In JSON mode, we don't have a running process to send to
        // Instead, we restart Claude with the appropriate permission flags
        Ok(())
    }

    /// Continue after permission response - no-op for JSON mode
    pub fn continue_after_permission<F>(&mut self, _choice: usize, mut _on_update: F) -> StreamingResult
    where
        F: FnMut(&str) -> Result<(), String>,
    {
        // In JSON mode, we restart the entire process with new permissions
        // This is a simplified implementation
        StreamingResult::Completed(StopReason::EndTurn, String::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_tool_name() {
        let session = StreamingSession::new(
            AgentInfo {
                id: "test".to_string(),
                name: "Test".to_string(),
                agent_type: crate::config::AgentType::Claude,
                capabilities: vec![],
                description: None,
                nickname: None,
                avatar: None,
                numeric_id: None,
                require_workdir: false,
                working_dir: None,
            },
            None,
        );

        // Test from meta
        let meta = serde_json::json!({"toolName": "write"});
        let raw = None;
        assert_eq!(
            session.resolve_tool_name(&Some(meta), &raw, "default"),
            Some("write".to_string())
        );

        // Test from raw input
        let meta = None;
        let raw = serde_json::json!({"tool": "read"});
        assert_eq!(
            session.resolve_tool_name(&meta, &Some(raw), "default"),
            Some("read".to_string())
        );

        // Test default fallback
        assert_eq!(
            session.resolve_tool_name(&None, &None, "default"),
            Some("default".to_string())
        );
    }
}
