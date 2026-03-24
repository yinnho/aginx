//! Streaming session support for ACP
//!
//! Converts Claude CLI output to ACP sessionUpdate notifications
//! Supports both JSON streaming mode (--output-format stream-json) and interactive mode

use std::io::{BufRead, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use uuid::Uuid;

use super::types::{
    AcpResponse, SessionUpdate, SessionUpdateParams, MessageContent,
    ToolCallStatus, ToolCallContent, ToolKind, StopReason,
};
use super::stream::{StreamEvent, ContentBlock as StreamContentBlock};
use crate::agent::{AgentInfo, PermissionPrompt, PermissionOption};

/// Permission request from streaming session
#[derive(Debug, Clone)]
pub struct StreamingPermissionRequest {
    /// Request ID
    pub request_id: String,
    /// Description
    pub description: String,
    /// Options
    pub options: Vec<PermissionOption>,
}

/// Streaming result - either continue with notifications or pause for permission
#[derive(Debug)]
pub enum StreamingResult {
    /// Completed successfully
    Completed(StopReason),
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
    /// Create a new streaming session (for standalone use)
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

    /// Create from existing session (for ACP with session manager)
    pub fn from_session(
        session_id: String,
        claude_session_uuid: String,
        agent_info: AgentInfo,
        workdir: Option<&str>,
    ) -> Self {
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

    /// Send a prompt with streaming and permission support
    ///
    /// This method runs Claude without stream-json to get permission prompts.
    /// Returns StreamingResult::PermissionNeeded when a permission is needed.
    pub fn send_prompt_interactive<F>(
        &mut self,
        message: &str,
        mut on_update: F,
    ) -> StreamingResult
    where
        F: FnMut(&str) -> Result<(), String>,
    {
        use crate::config::AgentType;

        match self.agent_info.agent_type {
            AgentType::Claude => self.send_prompt_claude_interactive(message, on_update),
            AgentType::Process => {
                // Process type doesn't support streaming permissions
                match self.send_prompt_process_streaming(message, on_update) {
                    Ok(reason) => StreamingResult::Completed(reason),
                    Err(e) => StreamingResult::Error(e),
                }
            }
            AgentType::Builtin => StreamingResult::Error("Builtin agents do not support streaming".to_string()),
        }
    }

    /// Continue after permission response
    ///
    /// Sends the permission choice and continues streaming
    pub fn continue_after_permission<F>(
        &mut self,
        choice: usize,
        mut on_update: F,
    ) -> StreamingResult
    where
        F: FnMut(&str) -> Result<(), String>,
    {
        // For now, we need to re-run the command with the choice as input
        // This is a simplified approach - in production, we'd keep the process alive
        self.send_permission_choice_streaming(choice, on_update)
    }

    /// Send prompt to Claude CLI with interactive mode (supports permissions)
    fn send_prompt_claude_interactive<F>(
        &mut self,
        message: &str,
        mut on_update: F,
    ) -> StreamingResult
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
        // Use --print without stream-json to get permission prompts
        cmd.arg("--print")
            .arg("--session-id")
            .arg(&self.claude_session_uuid)
            .env_remove("CLAUDECODE");

        if let Some(ref dir) = self.workdir {
            cmd.current_dir(dir);
        }

        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return StreamingResult::Error(format!("Failed to spawn claude: {}", e)),
        };

        // Write message to stdin
        if let Some(mut stdin) = child.stdin.take() {
            if let Err(e) = writeln!(stdin, "{}", message) {
                return StreamingResult::Error(format!("Write error: {}", e));
            }
            if let Err(e) = stdin.flush() {
                return StreamingResult::Error(format!("Flush error: {}", e));
            }
        }

        // Read output
        let stdout = match child.stdout.take() {
            Some(s) => s,
            None => return StreamingResult::Error("Failed to get stdout".to_string()),
        };
        let reader = std::io::BufReader::new(stdout);

        let mut output_buffer = String::new();
        let mut text_buffer = String::new();  // For extracting clean text from ANSI output

        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(e) => {
                    tracing::error!("Error reading line: {}", e);
                    continue;
                }
            };

            // Accumulate output for permission detection
            output_buffer.push_str(&line);
            output_buffer.push('\n');

            // Strip ANSI codes and extract clean text
            let clean_line = strip_ansi_codes(&line);

            // Skip empty lines
            if clean_line.trim().is_empty() {
                continue;
            }

            // Check for permission prompt
            if let Some(perm) = parse_permission_prompt_from_output(&output_buffer) {
                tracing::info!("Detected permission prompt: {}", perm.description);

                // Send what we have so far as message chunk
                if !text_buffer.is_empty() {
                    let notification = self.make_message_chunk(&text_buffer);
                    if let Err(e) = on_update(&notification) {
                        return StreamingResult::Error(e);
                    }
                    text_buffer.clear();
                }

                // Create channels for permission response
                let (response_tx, response_rx) = mpsc::channel();

                // Return permission needed
                return StreamingResult::PermissionNeeded {
                    request: StreamingPermissionRequest {
                        request_id: perm.request_id,
                        description: perm.description,
                        options: perm.options,
                    },
                    response_tx,
                    response_rx,
                };
            }

            // Send text as message chunk (skip tool-related output)
            if !is_tool_output(&clean_line) {
                text_buffer.push_str(&clean_line);
                text_buffer.push('\n');

                // Send chunk
                let notification = self.make_message_chunk(&clean_line);
                if let Err(e) = on_update(&notification) {
                    return StreamingResult::Error(e);
                }
            }
        }

        // Wait for process to complete
        let status = match child.wait() {
            Ok(s) => s,
            Err(e) => return StreamingResult::Error(format!("Wait error: {}", e)),
        };

        if !status.success() {
            // Read stderr for error message
            if let Some(stderr) = child.stderr.take() {
                let stderr_reader = std::io::BufReader::new(stderr);
                let error_msg: String = stderr_reader.lines()
                    .filter_map(|l| l.ok())
                    .collect::<Vec<_>>()
                    .join("\n");
                if !error_msg.is_empty() {
                    return StreamingResult::Error(format!("Claude error: {}", error_msg));
                }
            }
            return StreamingResult::Error("Claude process failed".to_string());
        }

        StreamingResult::Completed(StopReason::EndTurn)
    }

    /// Send permission choice and continue streaming
    fn send_permission_choice_streaming<F>(
        &mut self,
        choice: usize,
        mut on_update: F,
    ) -> StreamingResult
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
            .arg("--session-id")
            .arg(&self.claude_session_uuid)
            .env_remove("CLAUDECODE");

        if let Some(ref dir) = self.workdir {
            cmd.current_dir(dir);
        }

        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return StreamingResult::Error(format!("Failed to spawn claude: {}", e)),
        };

        // Write choice to stdin
        if let Some(mut stdin) = child.stdin.take() {
            if let Err(e) = writeln!(stdin, "{}", choice) {
                return StreamingResult::Error(format!("Write error: {}", e));
            }
            if let Err(e) = stdin.flush() {
                return StreamingResult::Error(format!("Flush error: {}", e));
            }
        }

        // Read output
        let stdout = match child.stdout.take() {
            Some(s) => s,
            None => return StreamingResult::Error("Failed to get stdout".to_string()),
        };
        let reader = std::io::BufReader::new(stdout);

        let mut output_buffer = String::new();

        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(e) => {
                    tracing::error!("Error reading line: {}", e);
                    continue;
                }
            };

            output_buffer.push_str(&line);
            output_buffer.push('\n');

            let clean_line = strip_ansi_codes(&line);

            if clean_line.trim().is_empty() {
                continue;
            }

            // Check for another permission prompt
            if let Some(perm) = parse_permission_prompt_from_output(&output_buffer) {
                tracing::info!("Detected another permission prompt: {}", perm.description);

                let (response_tx, response_rx) = mpsc::channel();

                return StreamingResult::PermissionNeeded {
                    request: StreamingPermissionRequest {
                        request_id: perm.request_id,
                        description: perm.description,
                        options: perm.options,
                    },
                    response_tx,
                    response_rx,
                };
            }

            // Send text as message chunk
            if !is_tool_output(&clean_line) {
                let notification = self.make_message_chunk(&clean_line);
                if let Err(e) = on_update(&notification) {
                    return StreamingResult::Error(e);
                }
            }
        }

        // Wait for process to complete
        let status = match child.wait() {
            Ok(s) => s,
            Err(e) => return StreamingResult::Error(format!("Wait error: {}", e)),
        };

        if !status.success() {
            if let Some(stderr) = child.stderr.take() {
                let stderr_reader = std::io::BufReader::new(stderr);
                let error_msg: String = stderr_reader.lines()
                    .filter_map(|l| l.ok())
                    .collect::<Vec<_>>()
                    .join("\n");
                if !error_msg.is_empty() {
                    return StreamingResult::Error(format!("Claude error: {}", error_msg));
                }
            }
            return StreamingResult::Error("Claude process failed".to_string());
        }

        StreamingResult::Completed(StopReason::EndTurn)
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

/// Strip ANSI escape codes from a string
fn strip_ansi_codes(s: &str) -> String {
    // Simple ANSI stripping - remove escape sequences
    let mut result = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == '\x1b' {
            // Start of escape sequence
            i += 1;
            if i < chars.len() && chars[i] == '[' {
                i += 1;
                // Skip until we hit a letter (end of sequence)
                while i < chars.len() && !chars[i].is_ascii_alphabetic() {
                    i += 1;
                }
                i += 1; // Skip the final letter
            }
        } else {
            result.push(chars[i]);
            i += 1;
        }
    }

    result
}

/// Check if a line is tool output (should be filtered from message chunks)
fn is_tool_output(line: &str) -> bool {
    // Tool output typically contains these patterns
    let tool_patterns = [
        "●", "○", "◉",  // Tool indicators
        "Tool use:", "Tool result:",
        "Reading:", "Writing:", "Editing:",
        "Executing:", "Running:",
        "✓", "✗",  // Success/failure indicators
    ];

    for pattern in &tool_patterns {
        if line.contains(pattern) {
            return true;
        }
    }

    false
}

/// Parse permission prompt from Claude CLI output
fn parse_permission_prompt_from_output(output: &str) -> Option<StreamingPermissionRequest> {
    let lines: Vec<&str> = output.lines().collect();
    let mut description_lines = Vec::new();
    let mut options = Vec::new();
    let mut found_prompt = false;
    let mut in_options = false;

    for line in &lines {
        let trimmed = line.trim();

        // Detect permission prompt keywords
        if !found_prompt {
            if trimmed.contains("Do you want") ||
               trimmed.contains("Allow") ||
               trimmed.contains("Proceed?") ||
               trimmed.contains("Continue?") ||
               (trimmed.starts_with("❯") && trimmed.contains("1.")) {
                found_prompt = true;
                // If line starts with ❯, it's an option line
                if trimmed.starts_with("❯") {
                    if let Some(opt) = parse_permission_option(trimmed) {
                        options.push(opt);
                        in_options = true;
                    }
                } else {
                    description_lines.push(trimmed.to_string());
                }
                continue;
            }
        }

        if found_prompt {
            // Detect option lines
            if let Some(opt) = parse_permission_option(trimmed) {
                in_options = true;
                options.push(opt);
            } else if in_options && trimmed.is_empty() {
                // Options ended
                break;
            } else if !in_options && !trimmed.is_empty() && !trimmed.starts_with("❯") {
                // Description continues
                description_lines.push(trimmed.to_string());
            } else if trimmed.starts_with("❯") || trimmed.starts_with("  ") {
                // Might be an option line
                if let Some(opt) = parse_permission_option(trimmed) {
                    options.push(opt);
                    in_options = true;
                }
            }
        }
    }

    if found_prompt && !options.is_empty() {
        Some(StreamingPermissionRequest {
            request_id: format!("perm_{}", Uuid::new_v4().simple()),
            description: if description_lines.is_empty() {
                "Permission required".to_string()
            } else {
                description_lines.join("\n")
            },
            options,
        })
    } else {
        None
    }
}

/// Parse a single permission option
fn parse_permission_option(line: &str) -> Option<PermissionOption> {
    use regex::Regex;

    // Remove leading symbols (❯, ●, ○, spaces)
    let line = line.trim_start_matches(['❯', '●', '○', ' ']).trim();

    // Match "1. xxx" or "1) xxx" format
    let re = regex::Regex::new(r"^(\d+)[.\)]\s*(.+)$").ok()?;

    if let Some(caps) = re.captures(line) {
        let index: usize = caps.get(1)?.as_str().parse().ok()?;
        let label = caps.get(2)?.as_str().trim().to_string();
        let is_default = line.starts_with('❯') || line.starts_with('●');

        Some(PermissionOption {
            index,
            label,
            is_default,
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_ansi_codes() {
        let input = "\x1b[32mHello\x1b[0m World";
        let result = strip_ansi_codes(input);
        assert_eq!(result, "Hello World");
    }

    #[test]
    fn test_parse_permission_option() {
        let line = "1. Yes, proceed";
        let opt = parse_permission_option(line).unwrap();
        assert_eq!(opt.index, 1);
        assert_eq!(opt.label, "Yes, proceed");
    }
}
