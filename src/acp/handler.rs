//! ACP Handler implementation
//!
//! Handles ACP protocol requests and routes them to appropriate handlers.

use std::sync::Arc;

use tokio::sync::{Mutex, mpsc};

use super::types::{AcpRequest, AcpResponse, InitializeParams, InitializeResult, NewSessionParams, LoadSessionParams, PromptParams, ContentBlock, StopReason};
use super::streaming::StreamingSession;
use crate::agent::{AgentManager, SessionManager, SendMessageResult};
use crate::binding::{self, BindResult};

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
            agents_dir: dirs::home_dir()
                .map(|h| h.join(".aginx").join("agents"))
                .unwrap_or_else(|| std::path::PathBuf::from(".aginx/agents")),
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


    /// Handle an ACP request
    pub async fn handle_request(&self, request: AcpRequest) -> AcpResponse {
        tracing::debug!("ACP request: {} ({:?})", request.method, request.id);

        match request.method.as_str() {
            "initialize" => self.handle_initialize(request).await,
            "newSession" => self.handle_new_session(request).await,
            "loadSession" => self.handle_load_session(request).await,
            "prompt" => self.handle_prompt(request).await,
            "cancel" => self.handle_cancel(request).await,
            "listSessions" => self.handle_list_sessions(request).await,
            "listAgents" => self.handle_list_agents(request).await,
            "discoverAgents" => self.handle_discover_agents(request).await,
            "registerAgent" => self.handle_register_agent(request).await,
            "bindDevice" => self.handle_bind_device(request).await,
            "listDirectory" => self.handle_list_directory(request).await,
            "readFile" => self.handle_read_file(request).await,
            "listConversations" => self.handle_list_conversations(request).await,
            "deleteConversation" => self.handle_delete_conversation(request).await,
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

        // 如果没有指定 agentId，使用配置中的第一个 agent
        let agent_id = match agent_id {
            Some(id) => id,
            None => {
                // 获取配置的第一个 agent
                let agents = self.agent_manager.list_agents().await;
                if agents.is_empty() {
                    return AcpResponse::error(request.id, -32602, "No agent configured");
                }
                agents[0].get("id").and_then(|v| v.as_str()).unwrap_or("echo").to_string()
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

        tracing::info!("ACP newSession: {} -> {}", session_id, agent_id);

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

        tracing::info!("ACP loadSession: {}", params.sessionId);

        // Check if session exists
        match self.session_manager.get_session_info(&params.sessionId).await {
            Some(info) => {
                // Session exists, return success with session info
                AcpResponse::success(request.id, serde_json::json!({
                    "sessionId": info.session_id,
                    "status": "resumed"
                }))
            }
            None => {
                // Session not found
                AcpResponse::error(request.id, 404, &format!("Session not found: {}", params.sessionId))
            }
        }
    }

    /// Handle prompt request
    async fn handle_prompt(&self, request: AcpRequest) -> AcpResponse {
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

        tracing::debug!("ACP prompt to session {}: {} chars", params.sessionId, message.len());

        // Send message to session
        let result = match self.session_manager.send_message(&params.sessionId, &message).await {
            Ok(r) => r,
            Err(e) => {
                return AcpResponse::error(request.id, -32603, &format!("Failed to send message: {}", e));
            }
        };

        match result {
            SendMessageResult::Response(response) => {
                AcpResponse::success(request.id, serde_json::json!({
                    "stopReason": "end_turn",
                    "response": response
                }))
            }
            _ => {
                AcpResponse::error(request.id, -32603, "Unexpected result type")
            }
        }
    }

    /// Handle prompt request with streaming support
    /// Sends streaming events via channel, returns final response
    pub async fn handle_prompt_streaming(
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
                return AcpResponse::error(request.id, -32603, &format!("Session not found: {}", params.sessionId));
            }
        };

        // Get agent info
        let agent_info = match self.agent_manager.get_agent_info(&session_info.agent_id).await {
            Some(agent) => agent,
            None => {
                return AcpResponse::error(request.id, -32603, &format!("Agent not found: {}", session_info.agent_id));
            }
        };

        // Use async streaming for real-time output
        let mut event_rx = match StreamingSession::run_claude_async(
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

        // Spawn task to process streaming events
        let session_id = params.sessionId.clone();
        let request_id = request.id.clone();
        let session_manager = self.session_manager.clone();
        let agent_id = session_info.agent_id.clone();

        tokio::spawn(async move {
            let mut final_result: Option<super::streaming::AsyncStreamingResult> = None;

            while let Some(event) = event_rx.recv().await {
                match event {
                    super::streaming::StreamingEvent::Notification(json) => {
                        // Immediately send notification to client
                        if tx.send(json).await.is_err() {
                            tracing::warn!("Client disconnected");
                            break;
                        }
                    }
                    super::streaming::StreamingEvent::Completed(result) => {
                        final_result = Some(result);
                        break;
                    }
                }
            }

            // Handle final result
            if let Some(result) = final_result {
                match result {
                    super::streaming::AsyncStreamingResult::Completed {
                        stop_reason,
                        content,
                        claude_session_id,
                    } => {
                        // Save claude_session_id for --resume
                        if let Some(ref sid) = claude_session_id {
                            if let Err(e) = session_manager.update_claude_session_id(&session_id, sid).await {
                                tracing::warn!("Failed to save claude_session_id: {}", e);
                            }
                        }

                        // Update persisted session metadata (UTF-8 safe truncation)
                        let msg_preview = if content.chars().count() > 100 {
                            format!("{}...", content.chars().take(100).collect::<String>())
                        } else {
                            content.clone()
                        };
                        session_manager.update_persisted_metadata(
                            &session_id,
                            &agent_id,
                            claude_session_id.as_deref(),
                            Some(&msg_preview),
                        );

                        // Send final response
                        let final_response = AcpResponse::success(request_id, serde_json::json!({
                            "stopReason": match stop_reason {
                                StopReason::EndTurn => "end_turn",
                                StopReason::Stop => "stop",
                                StopReason::Cancelled => "cancelled",
                                StopReason::Refusal => "refusal",
                                StopReason::Error => "error",
                            },
                            "response": content
                        }));
                        let _ = tx.send(serde_json::to_string(&final_response).unwrap_or_default()).await;
                    }
                    super::streaming::AsyncStreamingResult::Error(e) => {
                        tracing::error!("Streaming error: {}", e);
                        let error_response = AcpResponse::error(request_id, -32603, &format!("Streaming error: {}", e));
                        let _ = tx.send(serde_json::to_string(&error_response).unwrap_or_default()).await;
                    }
                }
            }
        });

        // Return immediately - streaming happens in background
        AcpResponse::success(request.id, serde_json::json!({
            "streaming": true,
            "sessionId": params.sessionId
        }))
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

        let sessions = self.session_manager.list_persisted_sessions(&agent_id);

        let conversations: Vec<serde_json::Value> = sessions.iter().map(|s| {
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
        let agent_id = match params.get("agentId").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => {
                return AcpResponse::error(request.id, -32602, "Missing agentId");
            }
        };

        // Close active session if exists
        let _ = self.session_manager.close_session(&session_id).await;

        // Delete persisted metadata
        self.session_manager.delete_persisted_session(&session_id, &agent_id);

        AcpResponse::success(request.id, serde_json::json!({
            "success": true
        }))
    }

    /// Handle discoverAgents request - scan directory for aginx.toml files
    async fn handle_discover_agents(&self, request: AcpRequest) -> AcpResponse {
        use std::path::PathBuf;
        use crate::agent::scan_directory;

        // Parse params
        let params = request.params.clone().unwrap_or(serde_json::json!({}));
        let scan_path = params.get("path")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| self.agents_dir.clone());

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
    async fn handle_register_agent(&self, request: AcpRequest) -> AcpResponse {
        use std::path::PathBuf;
        use crate::agent::{parse_aginx_toml, AgentConfig, AgentInfo};

        // Parse params
        let params = match &request.params {
            Some(p) => p.clone(),
            None => {
                return AcpResponse::error(request.id, -32602, "Missing params");
            }
        };

        // Either configPath or agent config is required
        let agent_info = if let Some(config_path) = params.get("configPath").and_then(|v| v.as_str()) {
            // Load from config file
            let path = PathBuf::from(config_path);
            let project_dir = path.parent().unwrap_or(&path).to_path_buf();

            match parse_aginx_toml(&path, &project_dir) {
                Ok(discovered) => {
                    AgentInfo::from(discovered.config)
                }
                Err(e) => {
                    return AcpResponse::error(request.id, -32602, &format!("Failed to parse config: {}", e));
                }
            }
        } else if let Some(agent_obj) = params.get("agent") {
            // Direct config
            match serde_json::from_value::<AgentConfig>(agent_obj.clone()) {
                Ok(config) => AgentInfo::from(config),
                Err(e) => {
                    return AcpResponse::error(request.id, -32602, &format!("Invalid agent config: {}", e));
                }
            }
        } else {
            return AcpResponse::error(request.id, -32602, "Missing configPath or agent");
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

        tracing::info!("ACP bindDevice: pairCode={}, deviceName={}", pair_code, device_name);

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

        let path = std::path::PathBuf::from(path_str);

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
async fn write_ndjson<W: tokio::io::AsyncWriteExt + Unpin>(writer: &mut W, line: &str) -> std::io::Result<()> {
    writer.write_all(line.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
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
                if method == "prompt" {
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

                        // Run streaming
                        let response = handler_clone.handle_prompt_streaming(request, tx).await;

                        // Wait for all notifications to be written
                        let _ = notify_task.await;

                        // Send final response
                        if let Ok(json) = response.to_ndjson() {
                            tracing::trace!("ACP prompt response: {}", json);
                            let mut w = writer_clone.lock().await;
                            let _ = write_ndjson(&mut *w, &json).await;
                        }
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
