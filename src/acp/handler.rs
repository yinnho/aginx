//! ACP Handler implementation
//!
//! Handles ACP protocol requests and routes them to appropriate handlers.

use std::sync::Arc;

use tokio::sync::mpsc;

use super::types::{AcpRequest, AcpResponse, InitializeParams, InitializeResult, NewSessionParams, LoadSessionParams, PromptParams, ContentBlock};
use super::streaming::{AsyncStreamingResult, StreamingEvent};
use super::ConnectionAuth;
use super::agent_event::{write_ndjson, stop_reason_str, truncate_preview};
use crate::agent::{AgentManager, SessionManager};
use crate::binding::{self, BindResult};

/// ACP Handler that processes ACP protocol messages
pub struct AcpHandler {
    /// Agent manager for getting agent info
    agent_manager: Arc<AgentManager>,
    /// Session manager for creating/managing sessions
    session_manager: Arc<SessionManager>,
    /// Default agents directory for discovery
    agents_dir: std::path::PathBuf,
    /// Global default access mode (from config)
    global_access: crate::config::AccessMode,
}

impl AcpHandler {
    /// Create a new ACP handler
    pub fn new(agent_manager: Arc<AgentManager>, session_manager: Arc<SessionManager>) -> Self {
        Self {
            agent_manager,
            session_manager,
            agents_dir: crate::config::agents_dir(),
            global_access: crate::config::AccessMode::default(),
        }
    }

    /// Create ACP handler with global access mode
    pub fn with_access(agent_manager: Arc<AgentManager>, session_manager: Arc<SessionManager>, agents_dir: std::path::PathBuf, global_access: crate::config::AccessMode) -> Self {
        Self {
            agent_manager,
            session_manager,
            agents_dir,
            global_access,
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
    pub async fn handle_request(&self, request: AcpRequest, auth: ConnectionAuth) -> (AcpResponse, bool) {
        tracing::info!("ACP request: {} ({:?}) auth={:?}", request.method, request.id, auth);

        let method = request.method.clone();

        // Methods always allowed regardless of auth
        match method.as_str() {
            "initialize" => return (self.handle_initialize(request).await, false),
            "bindDevice" => {
                let resp = self.handle_bind_device(request).await;
                // Only upgrade auth if binding actually succeeded (result.success == true)
                let upgrade = resp.result.as_ref()
                    .and_then(|r| r.get("success"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                return (resp, upgrade);
            }
            _ => {}
        }

        // For other methods: check auth
        match method.as_str() {
            "listAgents" => (self.handle_list_agents(request, auth).await, false),
            "session/new" => (self.handle_new_session_with_auth(request, auth).await, false),
            "session/load" | "loadSession" => {
                if auth == ConnectionAuth::Pending {
                    (AcpResponse::error(request.id, 401, "Authentication required"), false)
                } else {
                    (self.handle_load_session(request).await, false)
                }
            }
            "session/cancel" => {
                if auth == ConnectionAuth::Pending {
                    (AcpResponse::error(request.id, 401, "Authentication required"), false)
                } else {
                    (self.handle_cancel(request).await, false)
                }
            }
            "session/prompt" => {
                (AcpResponse::error(request.id, -32601, "Use streaming endpoint for session/prompt"), false)
            }
            // All other extension methods require authentication
            _ => {
                if auth == ConnectionAuth::Pending {
                    (AcpResponse::error(request.id, 401, "Authentication required"), false)
                } else {
                    (self.handle_request_internal(request).await, false)
                }
            }
        }
    }

    /// Internal handler for methods that require auth (already checked)
    async fn handle_request_internal(&self, request: AcpRequest) -> AcpResponse {
        match request.method.as_str() {
            "listConversations" => self.handle_list_conversations(request).await,
            "deleteConversation" => self.handle_delete_conversation(request).await,
            "getMessages" | "getConversationMessages" => self.handle_get_conversation_messages(request).await,
            "listAgents" => self.handle_list_agents(request, ConnectionAuth::Authenticated).await,
            "discoverAgents" => self.handle_discover_agents(request).await,
            "registerAgent" => self.handle_register_agent(request).await,
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

        // Check authToken from _meta
        let authenticated = params._meta
            .as_ref()
            .and_then(|m| m.get("authToken"))
            .and_then(|v| v.as_str())
            .map(|token| {
                let mgr = crate::binding::get_binding_manager();
                let mgr = mgr.lock().unwrap();
                mgr.verify_token(token).is_some()
            })
            .unwrap_or(false);

        if authenticated {
            tracing::info!("ACP initialize from {} v{} (authenticated)", params.clientInfo.name, params.clientInfo.version);
        } else {
            tracing::info!("ACP initialize from {} v{} (unauthenticated)", params.clientInfo.name, params.clientInfo.version);
        }

        let mut result = InitializeResult::new();
        result.authenticated = authenticated;
        AcpResponse::success(request.id, result)
    }

    /// Handle newSession request
    async fn handle_new_session(&self, request: AcpRequest, auth: ConnectionAuth) -> AcpResponse {
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

        // 如果没有指定 agentId，使用第一个可用的 agent（按 auth 过滤）
        let agent_id = match agent_id {
            Some(id) => id,
            None => {
                let agents = self.agent_manager.list_agents().await;
                let filtered: Vec<_> = if auth == ConnectionAuth::Authenticated {
                    agents
                } else {
                    agents.into_iter().filter(|a| {
                        a.get("access").and_then(|v| v.as_str()) == Some("public")
                    }).collect()
                };
                if filtered.is_empty() {
                    return AcpResponse::error(request.id, -32602, "No agent configured");
                }
                filtered[0].get("id").and_then(|v| v.as_str())
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
        let agent_session_id = &params.sessionId;
        tracing::info!("[LOAD] ACP loadSession: agent_session_id={}", agent_session_id);

        let agent_id = match &params.agentId {
            Some(id) => id.clone(),
            None => return AcpResponse::error(request.id, -32602, "agentId is required"),
        };

        let agent_info = match self.agent_manager.get_agent_info(&agent_id).await {
            Some(info) => info,
            None => return AcpResponse::error(request.id, 404, "Agent info not found"),
        };

        // Find the JSONL path to extract workdir
        let workdir = self.session_manager.find_workdir_for_session(
            agent_session_id,
            agent_info.storage_path.as_deref(),
        );

        // Create session with claude session ID as the aginx session ID too
        let session_id = match self.session_manager.create_session_with_agent_id(
            agent_session_id,
            &agent_info,
            workdir.as_deref(),
        ).await {
            Ok(id) => id,
            Err(e) => return AcpResponse::error(request.id, -32603, &format!("Failed to create session: {}", e)),
        };

        tracing::info!("ACP loadSession: session created {} for claude session {}", session_id, agent_session_id);

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
        auth: ConnectionAuth,
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

        // Check if session's agent is accessible
        if auth == ConnectionAuth::Pending {
            if let Some(agent_id) = self.get_session_agent_id(&params.sessionId).await {
                if !self.is_agent_public(&agent_id).await {
                    return AcpResponse::error(request.id, 401, "Authentication required for this agent");
                }
            }
        }

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
                                    agent_session_id,
                                } => {
                                    // Update persisted metadata
                                    let msg_preview = truncate_preview(&content, 100);
                                    session_manager.update_persisted_metadata(
                                        &session_id,
                                        &agent_id,
                                        agent_session_id.as_deref(),
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

            // 根据协议选择处理路径
            if agent_info.protocol == "claude-stream" {
                // Legacy 模式：每次启动新进程，使用 stream-json 输出格式
                tracing::info!("[PROMPT] Starting legacy streaming for session {}", params.sessionId);
                let mut event_rx = match super::streaming::StreamingSession::run_streaming_async(
                    params.sessionId.clone(),
                    agent_info,
                    session_info.workdir.clone(),
                    session_info.agent_session_id.clone(),
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
                                agent_session_id,
                            } => {
                                if let Some(ref sid) = agent_session_id {
                                    if let Err(e) = session_manager.update_agent_session_id(&session_id, sid).await {
                                        tracing::warn!("Failed to save agent_session_id: {}", e);
                                    }
                                }

                                let msg_preview = truncate_preview(&content, 100);
                                session_manager.update_persisted_metadata(
                                    &session_id,
                                    &agent_id,
                                    agent_session_id.as_deref(),
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
    async fn handle_list_agents(&self, request: AcpRequest, auth: ConnectionAuth) -> AcpResponse {
        let agents = self.agent_manager.list_agents().await;

        let filtered: Vec<serde_json::Value> = if auth == ConnectionAuth::Authenticated {
            agents
        } else {
            // Pending: only show public agents
            agents.into_iter().filter(|a| {
                a.get("access").and_then(|v| v.as_str()) == Some("public")
            }).collect()
        };

        AcpResponse::success(request.id, serde_json::json!({
            "agents": filtered,
            "nextCursor": null
        }))
    }

    /// Check if an agent is public
    async fn is_agent_public(&self, agent_id: &str) -> bool {
        let agent = self.agent_manager.get_agent_info(agent_id).await;
        match agent {
            Some(info) => matches!(info.access, crate::config::AccessMode::Public),
            None => false,
        }
    }

    /// Get the agent ID associated with a session
    async fn get_session_agent_id(&self, session_id: &str) -> Option<String> {
        self.session_manager.get_session_info(session_id).await
            .map(|s| s.agent_id.clone())
    }

    /// Handle new session with auth check
    async fn handle_new_session_with_auth(&self, request: AcpRequest, auth: ConnectionAuth) -> AcpResponse {
        let params: NewSessionParams = match &request.params {
            Some(p) => match serde_json::from_value(p.clone()) {
                Ok(params) => params,
                Err(e) => {
                    return AcpResponse::error(request.id, -32602, &format!("Invalid params: {}", e));
                }
            }
            None => NewSessionParams::default()
        };

        // Get agent ID
        let agent_id = params._meta
            .as_ref()
            .and_then(|m| m.get("agentId"))
            .and_then(|v| v.as_str())
            .map(String::from)
            .or_else(|| {
                request.params.as_ref()
                    .and_then(|p| p.get("agentId"))
                    .and_then(|v| v.as_str())
                    .map(String::from)
            });

        // Check agent access
        match agent_id {
            Some(ref aid) => {
                if auth == ConnectionAuth::Pending && !self.is_agent_public(aid).await {
                    return AcpResponse::error(request.id, 401, "Authentication required for this agent");
                }
            }
            None => {
                // No agent specified — Pending users can only use this if there's a public agent to fall back to
                if auth == ConnectionAuth::Pending {
                    let agents = self.agent_manager.list_agents().await;
                    let has_public = agents.iter().any(|a| a.get("access").and_then(|v| v.as_str()) == Some("public"));
                    if !has_public {
                        return AcpResponse::error(request.id, 401, "Authentication required");
                    }
                }
            }
        }

        // Delegate to existing handler — reconstruct request since we consumed params
        self.handle_new_session(request, auth).await
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

        // Check if agent has external session storage
        let agent_info = self.agent_manager.get_agent_info(&agent_id).await;
        let has_storage = agent_info.as_ref()
            .map(|i| i.storage_path.is_some())
            .unwrap_or(false);
        let storage_path = agent_info.as_ref()
            .and_then(|i| i.storage_path.clone());

        let conversations = if has_storage {
            // Agent has external session storage (e.g. ~/.claude/projects)
            self.session_manager.scan_external_sessions(storage_path.as_deref().unwrap_or(""), &agent_id)
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

        // session_id IS the agent's session ID — just delete the JSONL file directly
        let agent_id = params.get("agentId").and_then(|v| v.as_str());
        let storage_path = match agent_id {
            Some(aid) => self.agent_manager.get_agent_info(aid).await
                .and_then(|i| i.storage_path),
            None => None,
        };
        let deleted = self.session_manager.delete_jsonl_by_session_id(&session_id, storage_path.as_deref());

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

        // Get storage_path from agent config
        let agent_id = params.get("agentId").and_then(|v| v.as_str());
        let storage_path = match agent_id {
            Some(aid) => self.agent_manager.get_agent_info(aid).await
                .and_then(|i| i.storage_path),
            None => None,
        };
        let messages = self.session_manager.read_jsonl_messages_limited(&session_id, limit, storage_path.as_deref());

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
            Ok(discovered) => agent_config_to_info(discovered.config, &project_dir, &self.global_access),
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
                        let _response = handler_clone.handle_prompt(request, tx, ConnectionAuth::Authenticated).await;

                        // Wait for all notifications + final response to be written
                        let _ = notify_task.await;

                        // Don't send _response to stdout - already sent via tx
                    });

                    // Main loop continues immediately
                } else {
                    // Non-streaming methods
                    let (response, _) = handler.handle_request(request, ConnectionAuth::Authenticated).await;

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
