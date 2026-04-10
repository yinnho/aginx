//! ACP Handler implementation
//!
//! Handles ACP protocol requests and routes them to appropriate handlers.

use std::sync::Arc;

use tokio::sync::{Mutex, mpsc};

use super::types::{AcpRequest, AcpResponse, InitializeParams, InitializeResult, NewSessionParams, LoadSessionParams, PromptParams, ContentBlock};
use super::streaming::{AsyncStreamingResult, StreamingEvent};
use super::agent_event::{write_ndjson, stop_reason_str, truncate_preview};
use crate::agent::{AgentManager, SessionManager};
use crate::binding::{self, BindResult};
use crate::config::AgentType;

/// ACP Handler that processes ACP protocol messages
pub struct AcpHandler {
    /// Agent manager for getting agent info
    agent_manager: Arc<AgentManager>,
    /// Session manager for creating/managing sessions
    session_manager: Arc<SessionManager>,
    /// Initialize result (cached after first initialize)
    initialized: Mutex<bool>,
    /// Default agents directory for discovery
    agents_dir: std::path::PathBuf,
}

impl AcpHandler {
    /// Create a new ACP handler
    pub fn new(agent_manager: Arc<AgentManager>, session_manager: Arc<SessionManager>) -> Self {
        Self {
            agent_manager,
            session_manager,
            initialized: Mutex::new(false),
            agents_dir: crate::config::agents_dir(),
        }
    }

    /// Create ACP handler with custom agents directory
    pub fn with_agents_dir(agent_manager: Arc<AgentManager>, session_manager: Arc<SessionManager>, agents_dir: std::path::PathBuf) -> Self {
        Self {
            agent_manager,
            session_manager,
            initialized: Mutex::new(false),
            agents_dir,
        }
    }


    /// Safely resolve a path, preventing path traversal attacks.
    /// Returns the canonicalized path if valid, or an error message.
    fn safe_resolve_path(path_str: &str) -> Result<std::path::PathBuf, String> {
        let path = if path_str == "~" || path_str.is_empty() {
            dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("/"))
        } else if path_str.starts_with("~/") {
            let rest = &path_str[2..];
            dirs::home_dir()
                .map(|h| h.join(rest))
                .unwrap_or_else(|| std::path::PathBuf::from(path_str))
        } else {
            std::path::PathBuf::from(path_str)
        };

        // Canonicalize to resolve . and .. and symlinks
        let canonical = std::fs::canonicalize(&path)
            .map_err(|e| format!("Cannot resolve path: {}", e))?;

        // Ensure the path is within the home directory
        let home_dir = dirs::home_dir()
            .ok_or_else(|| "Cannot determine home directory".to_string())?;

        let home_canonical = std::fs::canonicalize(&home_dir)
            .map_err(|e| format!("Cannot resolve home directory: {}", e))?;

        // Use ancestors() for proper path containment check (prevents /Users/aliceevil bypassing /Users/alice)
        if canonical != home_canonical && !canonical.ancestors().any(|a| a == home_canonical) {
            return Err("Access denied: path outside home directory".to_string());
        }

        Ok(canonical)
    }

    /// Handle an ACP request
    pub async fn handle_request(&self, request: AcpRequest) -> AcpResponse {
        tracing::info!("ACP request: {} ({:?})", request.method, request.id);

        match request.method.as_str() {
            // 标准 ACP 核心方法
            "initialize" => self.handle_initialize(request).await,
            "session/new" => self.handle_new_session(request).await,
            "session/load" | "loadSession" => self.handle_load_session(request).await,
            "session/prompt" => {
                AcpResponse::error(request.id, -32601, "Use streaming endpoint for session/prompt")
            }
            "session/cancel" => self.handle_cancel(request).await,

            // Aginx 扩展方法
            "listConversations" => self.handle_list_conversations(request).await,
            "deleteConversation" => self.handle_delete_conversation(request).await,
            "getMessages" | "getConversationMessages" => self.handle_get_conversation_messages(request).await,
            "listAgents" => self.handle_list_agents(request).await,
            "discoverAgents" => self.handle_discover_agents(request).await,
            "registerAgent" => self.handle_register_agent(request).await,
            "bindDevice" => self.handle_bind_device(request).await,
            "listDirectory" => self.handle_list_directory(request).await,
            "readFile" => self.handle_read_file(request).await,
            "listSessions" => self.handle_list_sessions(request).await,
            _ => {
                tracing::warn!("Unknown ACP method: {}", request.method);
                AcpResponse::error(
                    request.id,
                    -32601,
                    &format!("Method not found: {}", request.method)
                )
            }
        }
    }

    /// Handle initialize request
    async fn handle_initialize(&self, request: AcpRequest) -> AcpResponse {
        let params: InitializeParams = match &request.params {
            Some(p) => match serde_json::from_value(p.clone()) {
                Ok(params) => params,
                Err(e) => {
                    return AcpResponse::error(request.id, -32602, &format!("Invalid params: {}", e));
                }
            }
            None => {
                return AcpResponse::error(request.id, -32602, "Missing params");
            }
        };

        tracing::info!("ACP initialize from {} v{}", params.clientInfo.name, params.clientInfo.version);

        // Mark as initialized
        {
            let mut init = self.initialized.lock().await;
            *init = true;
        }

        AcpResponse::success(request.id, InitializeResult::new())
    }

    /// Handle newSession request
    async fn handle_new_session(&self, request: AcpRequest) -> AcpResponse {
        let params: NewSessionParams = match &request.params {
            Some(p) => match serde_json::from_value(p.clone()) {
                Ok(params) => params,
                Err(e) => {
                    return AcpResponse::error(request.id, -32602, &format!("Invalid params: {}", e));
                }
            }
            None => NewSessionParams::default()
        };

        // Get agent ID from _meta or directly from params
        let agent_id = params._meta
            .as_ref()
            .and_then(|m| m.get("agentId"))
            .and_then(|v| v.as_str())
            .map(String::from)
            .or_else(|| {
                // Also try to get from params.agentId directly
                request.params.as_ref()
                    .and_then(|p| p.get("agentId"))
                    .and_then(|v| v.as_str())
                    .map(String::from)
            });

        // 如果没有指定 agentId，使用第一个可用的 agent
        let agent_id = match agent_id {
            Some(id) => id,
            None => {
                let agents = self.agent_manager.list_agents().await;
                if agents.is_empty() {
                    return AcpResponse::error(request.id, -32602, "No agent configured");
                }
                agents[0].get("id").and_then(|v| v.as_str())
                    .map(String::from)
                    .unwrap_or_else(|| "unknown".to_string())
            }
        };

        // Get agent info
        let agent_info = match self.agent_manager.get_agent_info(&agent_id).await {
            Some(info) => info,
            None => {
                return AcpResponse::error(request.id, -32602, &format!("Agent not found: {}", agent_id));
            }
        };

        // Create session
        let session_id = match self.session_manager.create_session(&agent_info, params.cwd.as_deref()).await {
            Ok(id) => id,
            Err(e) => {
                return AcpResponse::error(request.id, -32603, &format!("Failed to create session: {}", e));
            }
        };
        tracing::info!("ACP newSession: {} -> {:?}, streaming mode", session_id, agent_info.agent_type);

        AcpResponse::success(request.id, serde_json::json!({
            "sessionId": session_id
        }))
    }

    /// Handle loadSession request
    async fn handle_load_session(&self, request: AcpRequest) -> AcpResponse {
        let params: LoadSessionParams = match &request.params {
            Some(p) => match serde_json::from_value(p.clone()) {
                Ok(params) => params,
                Err(e) => {
                    return AcpResponse::error(request.id, -32602, &format!("Invalid params: {}", e));
                }
            }
            None => {
                return AcpResponse::error(request.id, -32602, "Missing params");
            }
        };

        // For Claude agents: session_id IS the Claude session ID
        // Just need to create an aginx session that uses --resume with this ID
        let claude_session_id = &params.sessionId;
        tracing::info!("[LOAD] ACP loadSession: claude_session_id={}", claude_session_id);

        // Look up agent info to get agent config
        let agents = self.agent_manager.list_agents().await;
        // Find the "claude" agent specifically (not just any agent)
        let agent_id = agents.iter()
            .filter_map(|a| {
                let agent_type = a.get("agent_type").or_else(|| a.get("type")).and_then(|v| v.as_str());
                let id = a.get("id").and_then(|v| v.as_str());
                if agent_type == Some("claude") { id } else { None }
            })
            .next()
            .map(String::from);

        let agent_id = match agent_id {
            Some(id) => id,
            None => return AcpResponse::error(request.id, 404, "No agent found"),
        };

        let agent_info = match self.agent_manager.get_agent_info(&agent_id).await {
            Some(info) => info,
            None => return AcpResponse::error(request.id, 404, "Agent info not found"),
        };

        // Find the JSONL path to extract workdir
        let workdir = self.session_manager.find_workdir_for_claude_session(claude_session_id);

        // Create session with claude session ID as the aginx session ID too
        let session_id = match self.session_manager.create_session_with_claude_id(
            claude_session_id,
            &agent_info,
            workdir.as_deref(),
        ).await {
            Ok(id) => id,
            Err(e) => return AcpResponse::error(request.id, -32603, &format!("Failed to create session: {}", e)),
        };

        tracing::info!("ACP loadSession: session created {} for claude session {}", session_id, claude_session_id);

        // Touch the session to prevent timeout cleanup while it's being used
        self.session_manager.touch_session(&session_id).await;

        AcpResponse::success(request.id, serde_json::json!({
            "sessionId": session_id,
            "status": "resumed"
        }))
    }

    /// Handle prompt request with streaming support
    /// Sends streaming events via channel, returns initial response immediately
    pub async fn handle_prompt(
        &self,
        request: AcpRequest,
        tx: mpsc::Sender<String>,
    ) -> AcpResponse {
        let params: PromptParams = match &request.params {
            Some(p) => match serde_json::from_value(p.clone()) {
                Ok(params) => params,
                Err(e) => {
                    return AcpResponse::error(request.id, -32602, &format!("Invalid params: {}", e));
                }
            }
            None => {
                return AcpResponse::error(request.id, -32602, "Missing params");
            }
        };

        // Extract text from prompt blocks
        let message: String = params.prompt.iter()
            .filter_map(|block| {
                if let ContentBlock::Text { text } = block {
                    Some(text.as_str())
                } else {
                    None
                }
            })
            .collect::<Vec<&str>>()
            .join("\n");

        if message.is_empty() {
            return AcpResponse::error(request.id, -32602, "Empty prompt");
        }

        tracing::debug!("ACP prompt (streaming) to session {}: {} chars", params.sessionId, message.len());

        // Get session info from session manager
        let session_info = match self.session_manager.get_session_info(&params.sessionId).await {
            Some(info) => info,
            None => {
                tracing::warn!("[PROMPT] Session not found: {} — caller may need to reload session", params.sessionId);
                return AcpResponse::error(request.id, -32603, &format!("Session not found: {}", params.sessionId));
            }
        };

        // Touch session to prevent timeout cleanup during prompt processing
        self.session_manager.touch_session(&params.sessionId).await;

        // Try to get ACP agent process (for Claude-type agents)
        let agent_process = self.session_manager.get_agent_process(&params.sessionId).await;

        if let Some(process) = agent_process {
            // === ACP Stdio mode: use persistent agent process ===
            let request_id = request.id.clone();
            let session_id = params.sessionId.clone();
            let agent_id = session_info.agent_id.clone();
            let session_manager = self.session_manager.clone();

            // Create streaming event channel
            let (event_tx, event_rx) = mpsc::channel::<StreamingEvent>(64);

            // Spawn task to run prompt via ACP process (takes ownership of event_tx)
            let process_clone = process.clone();
            let error_tx = tx.clone();
            let error_request_id = request_id.clone();
            let cleanup_session_id = session_id.clone();
            let cleanup_session_manager = self.session_manager.clone();
            tokio::spawn(async move {
                let mut p = process_clone.lock().await;
                if let Err(e) = p.prompt(&message, event_tx).await {
                    tracing::error!("ACP prompt error: {}", e);
                    // Clean up broken process so next prompt starts fresh
                    drop(p); // release lock before removing
                    cleanup_session_manager.remove_agent_process(&cleanup_session_id).await;
                    // send error response directly so client doesn't hang
                    let error_response = AcpResponse::error(error_request_id, -32603, &format!("Prompt error: {}", e));
                    let _ = error_tx.send(serde_json::to_string(&error_response).unwrap_or_default()).await;
                }
            });

            // Spawn task to forward streaming events to client
            let mut event_rx = event_rx;
            tokio::spawn(async move {
                while let Some(event) = event_rx.recv().await {
                    match event {
                        StreamingEvent::Notification(json) => {
                            if tx.send(json).await.is_err() {
                                tracing::warn!("Client disconnected");
                                break;
                            }
                        }
                        StreamingEvent::Completed(result) => {
                            match result {
                                AsyncStreamingResult::Completed {
                                    stop_reason,
                                    content,
                                    claude_session_id,
                                } => {
                                    // Update persisted metadata
                                    let msg_preview = truncate_preview(&content, 100);
                                    session_manager.update_persisted_metadata(
                                        &session_id,
                                        &agent_id,
                                        claude_session_id.as_deref(),
                                        Some(&msg_preview),
                                    );

                                    // Send final response
                                    let final_response = AcpResponse::success(request_id, serde_json::json!({
                                        "stopReason": stop_reason_str(&stop_reason),
                                        "response": content
                                    }));
                                    let _ = tx.send(serde_json::to_string(&final_response).unwrap_or_default()).await;
                                }
                                AsyncStreamingResult::Error(e) => {
                                    tracing::error!("Streaming error: {}", e);
                                    let error_response = AcpResponse::error(request_id, -32603, &format!("Streaming error: {}", e));
                                    let _ = tx.send(serde_json::to_string(&error_response).unwrap_or_default()).await;
                                }
                            }
                            break;
                        }
                    }
                }
            });

            // Return immediately - streaming happens in background via spawned tasks
            return AcpResponse::success(request.id, serde_json::json!({
                "streaming": true,
                "sessionId": params.sessionId
            }));
        } else {
            // === No active agent process: try to resume with --resume ===
            let agent_info = match self.agent_manager.get_agent_info(&session_info.agent_id).await {
                Some(agent) => agent,
                None => {
                    return AcpResponse::error(request.id, -32603, &format!("Agent not found: {}", session_info.agent_id));
                }
            };

            // 对于 Claude 类型和所有其他 agent，统一使用 legacy streaming 模式
            // 检查是否使用 legacy 模式 (args 包含 --print 表示使用 stream-json 输出)
            let use_legacy = agent_info.args.iter().any(|arg| arg == "--print");

            if use_legacy || agent_info.agent_type == AgentType::Claude {
                // Legacy 模式：每次启动新进程，使用 stream-json 输出格式
                tracing::info!("[PROMPT] Starting legacy streaming for session {}", params.sessionId);
                let mut event_rx = match super::streaming::StreamingSession::run_claude_async(
                    params.sessionId.clone(),
                    agent_info,
                    session_info.workdir.clone(),
                    session_info.claude_session_id.clone(),
                    &message,
                ).await {
                    Ok(rx) => rx,
                    Err(e) => {
                        return AcpResponse::error(request.id, -32603, &format!("Failed to start streaming: {}", e));
                    }
                };

                let session_id = params.sessionId.clone();
                let request_id = request.id.clone();
                let session_manager = self.session_manager.clone();
                let agent_id = session_info.agent_id.clone();
                let user_message = message.clone();

                // 先存储用户消息
                tracing::info!("[PROMPT] Appending user message to session {}", session_id);
                session_manager.append_message(&session_id, &agent_id, "user", &user_message);
                tracing::info!("[PROMPT] User message saved, spawning notification task");

                tokio::spawn(async move {
                    let mut final_result: Option<AsyncStreamingResult> = None;

                    while let Some(event) = event_rx.recv().await {
                        match event {
                            StreamingEvent::Notification(json) => {
                                tracing::info!("[PROMPT] Forwarding notification, json_len={}", json.len());
                                if tx.send(json).await.is_err() {
                                    tracing::warn!("Client disconnected");
                                    break;
                                }
                            }
                            StreamingEvent::Completed(result) => {
                                final_result = Some(result);
                                break;
                            }
                        }
                    }

                    if let Some(result) = final_result {
                        match result {
                            AsyncStreamingResult::Completed {
                                stop_reason,
                                content,
                                claude_session_id,
                            } => {
                                if let Some(ref sid) = claude_session_id {
                                    if let Err(e) = session_manager.update_claude_session_id(&session_id, sid).await {
                                        tracing::warn!("Failed to save claude_session_id: {}", e);
                                    }
                                }

                                let msg_preview = truncate_preview(&content, 100);
                                session_manager.update_persisted_metadata(
                                    &session_id,
                                    &agent_id,
                                    claude_session_id.as_deref(),
                                    Some(&msg_preview),
                                );

                                // 存储助手响应消息
                                session_manager.append_message(&session_id, &agent_id, "assistant", &content);

                                let final_response = AcpResponse::success(request_id, serde_json::json!({
                                    "stopReason": stop_reason_str(&stop_reason),
                                    "response": content
                                }));
                                let _ = tx.send(serde_json::to_string(&final_response).unwrap_or_default()).await;
                            }
                            AsyncStreamingResult::Error(e) => {
                                tracing::error!("Streaming error: {}", e);
                                let error_response = AcpResponse::error(request_id, -32603, &format!("Streaming error: {}", e));
                                let _ = tx.send(serde_json::to_string(&error_response).unwrap_or_default()).await;
                            }
                        }
                    }
                });

                // Return immediately - streaming happens in background
                return AcpResponse::success(request.id, serde_json::json!({
                    "streaming": true,
                    "sessionId": params.sessionId
                }));
            } else {
                // 非 Claude/legacy 类型 agent：使用 stdin/stdout 进程模式
                tracing::info!("[PROMPT] Starting process streaming for session {}", params.sessionId);

                let command = agent_info.command.clone();
                let args: Vec<String> = agent_info.args.iter()
                    .map(|arg| arg.replace("${SESSION_ID}", &params.sessionId))
                    .collect();
                let env = agent_info.env.clone();
                let workdir = session_info.workdir.clone().or(agent_info.working_dir.clone());
                let request_id = request.id.clone();
                let session_id = params.sessionId.clone();

                tokio::spawn(async move {
                    let mut cmd = tokio::process::Command::new(&command);
                    cmd.args(&args)
                        .stdin(std::process::Stdio::piped())
                        .stdout(std::process::Stdio::piped());

                    if let Some(dir) = &workdir {
                        cmd.current_dir(dir);
                    }
                    for (k, v) in &env {
                        cmd.env(k, v);
                    }

                    match cmd.spawn() {
                        Ok(mut child) => {
                            // Write message to stdin
                            if let Some(mut stdin) = child.stdin.take() {
                                use tokio::io::AsyncWriteExt;
                                let _ = stdin.write_all(message.as_bytes()).await;
                                let _ = stdin.write_all(b"\n").await;
                                drop(stdin); // Close stdin to signal EOF
                            }

                            // Read stdout
                            let mut full_output = String::new();
                            if let Some(stdout) = child.stdout.take() {
                                use tokio::io::AsyncBufReadExt;
                                let reader = tokio::io::BufReader::new(stdout);
                                let mut lines = reader.lines();

                                while let Ok(Some(line)) = lines.next_line().await {
                                    full_output.push_str(&line);
                                    full_output.push('\n');

                                    // Send as chunk notification
                                    let notification = serde_json::json!({
                                        "jsonrpc": "2.0",
                                        "method": "sessionUpdate",
                                        "params": {
                                            "sessionId": session_id,
                                            "update": {
                                                "sessionUpdate": "agent_message_chunk",
                                                "content": {"type": "text", "text": line}
                                            }
                                        }
                                    });
                                    if tx.send(serde_json::to_string(&notification).unwrap_or_default()).await.is_err() {
                                        break;
                                    }
                                }
                            }

                            // Always wait for child to prevent orphan processes
                            let _ = child.wait().await;

                            // Send final response
                            let final_response = AcpResponse::success(request_id, serde_json::json!({
                                "stopReason": "end_turn",
                                "response": full_output.trim_end()
                            }));
                            let _ = tx.send(serde_json::to_string(&final_response).unwrap_or_default()).await;
                        }
                        Err(e) => {
                            let error_response = AcpResponse::error(request_id, -32603, &format!("Failed to start process: {}", e));
                            let _ = tx.send(serde_json::to_string(&error_response).unwrap_or_default()).await;
                        }
                    }
                });

                return AcpResponse::success(request.id, serde_json::json!({
                    "streaming": true,
                    "sessionId": params.sessionId
                }));
            }
        }
    }

    /// Handle cancel request
    async fn handle_cancel(&self, request: AcpRequest) -> AcpResponse {
        use super::types::CancelParams;

        let params: CancelParams = match &request.params {
            Some(p) => match serde_json::from_value(p.clone()) {
                Ok(params) => params,
                Err(e) => {
                    return AcpResponse::error(request.id, -32602, &format!("Invalid params: {}", e));
                }
            }
            None => {
                return AcpResponse::error(request.id, -32602, "Missing params");
            }
        };

        tracing::info!("ACP cancel: sessionId={}", params.sessionId);

        // Try to close the session
        match self.session_manager.close_session(&params.sessionId).await {
            Ok(()) => {
                AcpResponse::success(request.id, serde_json::json!({
                    "cancelled": true,
                    "sessionId": params.sessionId
                }))
            }
            Err(e) => {
                // Session might not exist or already closed
                tracing::warn!("Cancel failed for session {}: {}", params.sessionId, e);
                AcpResponse::success(request.id, serde_json::json!({
                    "cancelled": false,
                    "reason": e
                }))
            }
        }
    }

    /// Handle listSessions request
    async fn handle_list_sessions(&self, request: AcpRequest) -> AcpResponse {
        // Get agent_id from params if provided
        let params = request.params.clone().unwrap_or(serde_json::json!({}));
        let agent_id = params.get("agentId")
            .and_then(|v| v.as_str());

        let sessions = if let Some(aid) = agent_id {
            self.session_manager.list_persisted_sessions(aid)
        } else {
            // Return all sessions from all agents
            let agents = self.agent_manager.list_agents().await;
            let mut all_sessions = Vec::new();
            for agent in &agents {
                if let Some(id) = agent.get("id").and_then(|v| v.as_str()) {
                    all_sessions.extend(self.session_manager.list_persisted_sessions(id));
                }
            }
            all_sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
            all_sessions
        };

        let session_list: Vec<serde_json::Value> = sessions.iter().map(|s| {
            serde_json::json!({
                "sessionId": s.session_id,
                "agentId": s.agent_id,
                "workdir": s.workdir,
                "title": s.title,
                "lastMessage": s.last_message,
                "createdAt": s.created_at,
                "updatedAt": s.updated_at
            })
        }).collect();

        AcpResponse::success(request.id, serde_json::json!({
            "sessions": session_list,
            "nextCursor": null
        }))
    }

    /// Handle listAgents request
    async fn handle_list_agents(&self, request: AcpRequest) -> AcpResponse {
        let agents = self.agent_manager.list_agents().await;

        AcpResponse::success(request.id, serde_json::json!({
            "agents": agents,
            "nextCursor": null
        }))
    }

    /// Handle listConversations request - list persisted sessions for an agent
    async fn handle_list_conversations(&self, request: AcpRequest) -> AcpResponse {
        let params = request.params.clone().unwrap_or(serde_json::json!({}));
        let agent_id = match params.get("agentId").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => {
                return AcpResponse::error(request.id, -32602, "Missing agentId");
            }
        };

        // Check if this is a claude-type agent
        let agent_info = self.agent_manager.get_agent_info(&agent_id).await;
        let is_claude = agent_info.as_ref()
            .map(|i| i.agent_type == AgentType::Claude)
            .unwrap_or(false);

        let conversations = if is_claude {
            // Direct scan of Claude CLI JSONL sessions — no aginx metadata layer
            self.session_manager.scan_claude_sessions_direct()
        } else {
            // Non-Claude agents: use persisted sessions
            let sessions = self.session_manager.list_persisted_sessions(&agent_id);
            sessions.iter().map(|s| {
                serde_json::json!({
                    "sessionId": s.session_id,
                    "agentId": s.agent_id,
                    "workdir": s.workdir,
                    "title": s.title,
                    "lastMessage": s.last_message,
                    "createdAt": s.created_at,
                    "updatedAt": s.updated_at
                })
            }).collect()
        };

        AcpResponse::success(request.id, serde_json::json!({
            "conversations": conversations
        }))
    }

    /// Handle deleteConversation request
    async fn handle_delete_conversation(&self, request: AcpRequest) -> AcpResponse {
        let params = request.params.clone().unwrap_or(serde_json::json!({}));
        let session_id = match params.get("sessionId").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => {
                return AcpResponse::error(request.id, -32602, "Missing sessionId");
            }
        };

        // session_id IS the Claude session ID — just delete the JSONL file directly
        let deleted = self.session_manager.delete_claude_jsonl_by_session_id(&session_id);

        tracing::info!("Delete conversation {}: {}", session_id, if deleted { "success" } else { "not found" });

        AcpResponse::success(request.id, serde_json::json!({
            "success": deleted
        }))
    }

    /// Handle getConversationMessages - read messages directly from Claude JSONL
    async fn handle_get_conversation_messages(&self, request: AcpRequest) -> AcpResponse {
        let params = request.params.clone().unwrap_or(serde_json::json!({}));
        let session_id = match params.get("sessionId").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => {
                return AcpResponse::error(request.id, -32602, "Missing sessionId");
            }
        };
        let limit = params.get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(10) as usize;

        tracing::info!("[getConversationMessages] session_id={}, limit={}", session_id, limit);

        // session_id IS the Claude session ID — read directly from JSONL
        let messages = self.session_manager.read_claude_jsonl_messages_limited(&session_id, limit);

        tracing::info!("[getConversationMessages] Returning {} messages", messages.len());

        AcpResponse::success(request.id, serde_json::json!({
            "messages": messages
        }))
    }

    /// Handle discoverAgents request - scan directory for aginx.toml files
    async fn handle_discover_agents(&self, request: AcpRequest) -> AcpResponse {
        use crate::agent::scan_directory;

        // Parse params
        let params = request.params.clone().unwrap_or(serde_json::json!({}));
        let scan_path = match params.get("path").and_then(|v| v.as_str()) {
            Some(p) => match Self::safe_resolve_path(p) {
                Ok(resolved) => resolved,
                Err(e) => return AcpResponse::error(request.id, 400, &e),
            },
            None => self.agents_dir.clone(),
        };

        let max_depth = params.get("maxDepth")
            .and_then(|v| v.as_u64())
            .unwrap_or(5) as usize;

        tracing::info!("Scanning for agents in: {} (max depth: {})", scan_path.display(), max_depth);

        // Scan directory
        let discovered = scan_directory(&scan_path, max_depth);

        // Convert to JSON - check registration for each agent
        let mut agents = Vec::new();
        for agent in &discovered {
            let registered = self.agent_manager.get_agent_info(&agent.config.id).await.is_some();
            agents.push(serde_json::json!({
                "id": agent.config.id,
                "name": agent.config.name,
                "agentType": agent.config.agent_type,
                "version": agent.config.version,
                "description": agent.config.description,
                "configPath": agent.config_path.to_string_lossy(),
                "projectDir": agent.project_dir.to_string_lossy(),
                "available": agent.available,
                "error": agent.error,
                "registered": registered
            }));
        }

        tracing::info!("Discovered {} agents", agents.len());

        AcpResponse::success(request.id, serde_json::json!({
            "agents": agents,
            "scanPath": scan_path.to_string_lossy(),
            "nextCursor": null
        }))
    }

    /// Handle registerAgent request - register a discovered agent
    /// Only allows registration via local configPath (prevents remote command injection)
    async fn handle_register_agent(&self, request: AcpRequest) -> AcpResponse {
        use crate::agent::{parse_aginx_toml, agent_config_to_info};

        // Parse params
        let params = match &request.params {
            Some(p) => p.clone(),
            None => {
                return AcpResponse::error(request.id, -32602, "Missing params");
            }
        };

        let config_path = match params.get("configPath").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => {
                return AcpResponse::error(request.id, -32602, "configPath is required");
            }
        };

        // Validate path is within home directory
        let path = match Self::safe_resolve_path(config_path) {
            Ok(p) => p,
            Err(e) => return AcpResponse::error(request.id, 400, &e),
        };

        let project_dir = path.parent().unwrap_or(&path).to_path_buf();

        let agent_info = match parse_aginx_toml(&path, &project_dir) {
            Ok(discovered) => agent_config_to_info(discovered.config, &project_dir),
            Err(e) => {
                return AcpResponse::error(request.id, -32602, &format!("Failed to parse config: {}", e));
            }
        };

        // Register agent
        let agent_id = agent_info.id.clone();
        self.agent_manager.register_agent(agent_info).await;
        tracing::info!("Registered agent: {}", agent_id);

        AcpResponse::success(request.id, serde_json::json!({
            "agentId": agent_id,
            "success": true
        }))
    }

    /// Handle bindDevice request
    async fn handle_bind_device(&self, request: AcpRequest) -> AcpResponse {
        let params = match &request.params {
            Some(p) => p,
            None => {
                return AcpResponse::error(request.id, -32602, "Missing params");
            }
        };

        let pair_code = params.get("pairCode")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let device_name = params.get("deviceName")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown Device");

        if pair_code.is_empty() {
            return AcpResponse::error(request.id, -32602, "pairCode is required");
        }

        tracing::debug!("ACP bindDevice: pairCode={}, deviceName={}", pair_code, device_name);

        // 使用单例 BindingManager
        let manager = binding::get_binding_manager();
        let mut binding_manager = manager.lock().unwrap();
        match binding_manager.bind_device(pair_code, device_name) {
            BindResult::Success(device) => {
                tracing::info!("Device bound successfully: {}", device.id);
                AcpResponse::success(request.id, serde_json::json!({
                    "success": true,
                    "deviceId": device.id,
                    "token": device.token
                }))
            }
            BindResult::AlreadyBound { device_name } => {
                tracing::warn!("Already bound to device: {}", device_name);
                AcpResponse::error(request.id, 409, &format!("Already bound to device: {}", device_name))
            }
            BindResult::InvalidCode => {
                tracing::warn!("Invalid or expired pair code: {}", pair_code);
                AcpResponse::error(request.id, 400, "Invalid or expired pair code")
            }
        }
    }

    /// Handle listDirectory request - browse server filesystem
    async fn handle_list_directory(&self, request: AcpRequest) -> AcpResponse {
        let params = match &request.params {
            Some(p) => p.clone(),
            None => serde_json::json!({}),
        };

        let path_str = params.get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("~");

        let path = match Self::safe_resolve_path(path_str) {
            Ok(p) => p,
            Err(e) => {
                return AcpResponse::error(request.id, 400, &e);
            }
        };

        if !path.exists() {
            return AcpResponse::error(request.id, 404, &format!("Path not found: {}", path.display()));
        }

        if !path.is_dir() {
            return AcpResponse::error(request.id, 400, &format!("Not a directory: {}", path.display()));
        }

        let mut entries = Vec::new();
        match std::fs::read_dir(&path) {
            Ok(dir_entries) => {
                for entry in dir_entries.flatten() {
                    let file_name = entry.file_name().to_string_lossy().to_string();
                    // Skip entries that can't be read
                    let metadata = match entry.metadata() {
                        Ok(m) => m,
                        Err(_) => continue,
                    };

                    let entry_type = if metadata.is_dir() {
                        "directory"
                    } else {
                        "file"
                    };

                    let modified = metadata.modified().ok()
                        .map(|t| t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs());

                    entries.push(serde_json::json!({
                        "name": file_name,
                        "type": entry_type,
                        "size": if metadata.is_file() { Some(metadata.len()) } else { None },
                        "modified": modified,
                        "isHidden": file_name.starts_with('.')
                    }));
                }
            }
            Err(e) => {
                return AcpResponse::error(request.id, 403, &format!("Cannot read directory: {}", e));
            }
        }

        // Sort: directories first, then alphabetically
        entries.sort_by(|a, b| {
            let a_is_dir = a.get("type").and_then(|v| v.as_str()) == Some("directory");
            let b_is_dir = b.get("type").and_then(|v| v.as_str()) == Some("directory");
            match (a_is_dir, b_is_dir) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => {
                    let a_name = a.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let b_name = b.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    a_name.to_lowercase().cmp(&b_name.to_lowercase())
                }
            }
        });

        AcpResponse::success(request.id, serde_json::json!({
            "path": path.to_string_lossy(),
            "entries": entries
        }))
    }

    /// Handle readFile request - read a file from server filesystem
    async fn handle_read_file(&self, request: AcpRequest) -> AcpResponse {
        let params = match &request.params {
            Some(p) => p,
            None => {
                return AcpResponse::error(request.id, -32602, "Missing params");
            }
        };

        let path_str = match params.get("path").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => {
                return AcpResponse::error(request.id, -32602, "Missing path");
            }
        };

        let path = match Self::safe_resolve_path(path_str) {
            Ok(p) => p,
            Err(e) => {
                return AcpResponse::error(request.id, 400, &e);
            }
        };

        if !path.exists() {
            return AcpResponse::error(request.id, 404, &format!("File not found: {}", path.display()));
        }

        if path.is_dir() {
            return AcpResponse::error(request.id, 400, &format!("Path is a directory: {}", path.display()));
        }

        let metadata = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(e) => {
                return AcpResponse::error(request.id, 403, &format!("Cannot read file metadata: {}", e));
            }
        };

        // Limit file size to 10MB
        const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024;
        if metadata.len() > MAX_FILE_SIZE {
            return AcpResponse::error(request.id, 413, &format!("File too large: {} bytes (max 10MB)", metadata.len()));
        }

        match std::fs::read(&path) {
            Ok(bytes) => {
                use base64::Engine;
                let content = base64::engine::general_purpose::STANDARD.encode(&bytes);

                // Guess MIME type from extension
                let mime_type = path.extension()
                    .and_then(|e| e.to_str())
                    .map(|ext| match ext {
                        "txt" | "md" | "toml" | "json" | "yaml" | "yml" | "xml" | "csv" | "log" | "rs" | "py" | "js" | "ts" | "kt" | "java" | "c" | "h" | "cpp" | "go" | "sh" | "bash" => format!("text/{}", ext),
                        "html" | "htm" => "text/html".to_string(),
                        "css" => "text/css".to_string(),
                        "png" => "image/png".to_string(),
                        "jpg" | "jpeg" => "image/jpeg".to_string(),
                        "gif" => "image/gif".to_string(),
                        "svg" => "image/svg+xml".to_string(),
                        "pdf" => "application/pdf".to_string(),
                        "zip" => "application/zip".to_string(),
                        _ => "application/octet-stream".to_string(),
                    })
                    .unwrap_or_else(|| "application/octet-stream".to_string());

                let file_name = path.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("unknown")
                    .to_string();

                AcpResponse::success(request.id, serde_json::json!({
                    "name": file_name,
                    "size": bytes.len(),
                    "content": content,
                    "mimeType": mime_type
                }))
            }
            Err(e) => {
                AcpResponse::error(request.id, 403, &format!("Cannot read file: {}", e))
            }
        }
    }
}
/// Run ACP protocol over stdin/stdout with streaming support
pub async fn run_acp_stdio(agent_manager: Arc<AgentManager>, session_manager: Arc<SessionManager>) {
    let handler = Arc::new(AcpHandler::new(agent_manager, session_manager));

    tracing::info!("Starting ACP stdio mode");

    use tokio::io::AsyncBufReadExt;

    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let mut reader = tokio::io::BufReader::new(stdin);
    let writer = Arc::new(tokio::sync::Mutex::new(tokio::io::BufWriter::new(stdout)));
    let mut line = String::new();

    loop {
        line.clear();

        match reader.read_line(&mut line).await {
            Ok(0) => {
                // EOF
                tracing::debug!("ACP stdin closed");
                break;
            }
            Ok(_) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }

                tracing::trace!("ACP received: {}", line);

                // Parse request
                let request: AcpRequest = match serde_json::from_str(line) {
                    Ok(req) => req,
                    Err(e) => {
                        tracing::error!("Failed to parse ACP request: {}", e);
                        let response = AcpResponse::error(None, -32700, &format!("Parse error: {}", e));
                        if let Ok(json) = response.to_ndjson() {
                            let mut w = writer.lock().await;
                            let _ = write_ndjson(&mut *w, &json).await;
                        }
                        continue;
                    }
                };

                // Handle request with streaming support
                let method = request.method.clone();
                if method == "prompt" || method == "session/prompt" {
                    // SPAWN: run streaming in a separate task so the main loop
                    // can continue reading the next request (e.g. permissionResponse)
                    let (tx, rx) = mpsc::channel::<String>(32);
                    let handler_clone = handler.clone();
                    let writer_clone = writer.clone();

                    tokio::spawn(async move {
                        // Notification forwarder
                        let writer_notify = writer_clone.clone();
                        let notify_task = tokio::spawn(async move {
                            let mut rx = rx;
                            while let Some(notification) = rx.recv().await {
                                let mut w = writer_notify.lock().await;
                                if let Err(e) = write_ndjson(&mut *w, &notification).await {
                                    tracing::error!("Failed to write notification: {}", e);
                                    break;
                                }
                            }
                        });

                        // Run streaming (final response is sent through tx channel)
                        let _response = handler_clone.handle_prompt(request, tx).await;

                        // Wait for all notifications + final response to be written
                        let _ = notify_task.await;

                        // Don't send _response to stdout - already sent via tx
                    });

                    // Main loop continues immediately
                } else {
                    // Non-streaming methods
                    let response = handler.handle_request(request).await;

                    // Send response
                    if let Ok(json) = response.to_ndjson() {
                        tracing::trace!("ACP response: {}", json);
                        let mut w = writer.lock().await;
                        let _ = write_ndjson(&mut *w, &json).await;
                    }
                }
            }
            Err(e) => {
                tracing::error!("Failed to read from stdin: {}", e);
                break;
            }
        }
    }

    tracing::info!("ACP stdio mode ended");
}
