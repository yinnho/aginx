//! ACP Handler implementation
//!
//! Handles ACP protocol requests and routes them to appropriate handlers.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{Mutex, mpsc, RwLock};

use super::types::{AcpRequest, AcpResponse, InitializeParams, InitializeResult, NewSessionParams, LoadSessionParams, PromptParams, ContentBlock};
use super::streaming::StreamingSession;
use crate::agent::{AgentManager, SessionManager, SendMessageResult};
use crate::binding::{self, BindResult};

/// Permission choice sender for responding to pending permission requests
#[derive(Clone)]
pub struct PermissionChoiceSender {
    pub tx: std::sync::mpsc::Sender<usize>,
}

/// ACP Handler that processes ACP protocol messages
pub struct AcpHandler {
    /// Agent manager for getting agent info
    agent_manager: Arc<AgentManager>,
    /// Session manager for creating/managing sessions
    session_manager: Arc<SessionManager>,
    /// Initialize result (cached after first initialize)
    initialized: Mutex<bool>,
    /// Pending permission senders (session_id -> sender)
    pending_permissions: RwLock<HashMap<String, PermissionChoiceSender>>,
}

impl AcpHandler {
    /// Create a new ACP handler
    pub fn new(agent_manager: Arc<AgentManager>, session_manager: Arc<SessionManager>) -> Self {
        Self {
            agent_manager,
            session_manager,
            initialized: Mutex::new(false),
            pending_permissions: RwLock::new(HashMap::new()),
        }
    }

    /// Store a permission sender for a session
    pub async fn store_permission_sender(&self, session_id: &str, sender: PermissionChoiceSender) {
        let mut permissions = self.pending_permissions.write().await;
        permissions.insert(session_id.to_string(), sender);
    }

    /// Get and remove a permission sender for a session
    pub async fn take_permission_sender(&self, session_id: &str) -> Option<PermissionChoiceSender> {
        let mut permissions = self.pending_permissions.write().await;
        permissions.remove(session_id)
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
            "permissionResponse" => self.handle_permission_response(request).await,
            "bindDevice" => self.handle_bind_device(request).await,
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

    /// Handle prompt request (returns result for streaming handler to process)
    async fn handle_prompt(&self, request: AcpRequest) -> AcpResponse {
        // Default implementation without notification support
        self.handle_prompt_with_notification(request, None).await
    }

    /// Handle prompt request with notification support for permissions
    async fn handle_prompt_with_notification(
        &self,
        request: AcpRequest,
        notification_tx: Option<mpsc::Sender<String>>,
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

        tracing::debug!("ACP prompt to session {}: {} chars", params.sessionId, message.len());

        // Send message to session
        let result = match self.session_manager.send_message(&params.sessionId, &message).await {
            Ok(r) => r,
            Err(e) => {
                return AcpResponse::error(request.id, -32603, &format!("Failed to send message: {}", e));
            }
        };

        // Handle result
        match result {
            SendMessageResult::Response(response) => {
                // Return success with end_turn and response content
                AcpResponse::success(request.id, serde_json::json!({
                    "stopReason": "end_turn",
                    "response": response
                }))
            }
            SendMessageResult::PermissionNeeded(prompt) => {
                // Send requestPermission notification if channel available
                if let Some(tx) = notification_tx {
                    let notification = AcpResponse::notification("requestPermission", serde_json::json!({
                        "requestId": prompt.request_id,
                        "description": prompt.description,
                        "options": prompt.options.iter().enumerate().map(|(i, opt)| {
                            serde_json::json!({
                                "optionId": format!("opt_{}", i),
                                "label": opt.label
                            })
                        }).collect::<Vec<_>>()
                    }));

                    if let Ok(json) = notification.to_ndjson() {
                        if let Err(e) = tx.send(json).await {
                            tracing::error!("Failed to send permission notification: {}", e);
                        }
                    }
                }

                // Return with permission_required stop reason
                AcpResponse::success(request.id, serde_json::json!({
                    "stopReason": "permission_required",
                    "permissionRequest": {
                        "requestId": prompt.request_id,
                        "description": prompt.description,
                        "options": prompt.options.iter().enumerate().map(|(i, opt)| {
                            serde_json::json!({
                                "optionId": format!("opt_{}", i),
                                "label": opt.label
                            })
                        }).collect::<Vec<_>>()
                    }
                }))
            }
            SendMessageResult::PermissionHandled { .. } => {
                AcpResponse::success(request.id, serde_json::json!({
                    "stopReason": "end_turn"
                }))
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

        // Create streaming session from existing session
        let session_uuid = session_info.session_uuid
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        let mut streaming_session = StreamingSession::from_session(
            params.sessionId.clone(),
            session_uuid,
            agent_info,
            session_info.workdir.as_deref(),
        );

        // For true streaming, we use spawn_blocking because StreamingSession uses blocking I/O
        // We use a sync channel within the blocking task and relay to async channel
        let (sync_tx, sync_rx) = std::sync::mpsc::channel::<String>();
        let (perm_tx, perm_rx) = std::sync::mpsc::channel::<usize>();
        let message_inner = message.clone();
        let session_id_inner = params.sessionId.clone();

        // Store permission sender so it can be used by handle_permission_response
        let permission_sender = PermissionChoiceSender { tx: perm_tx };
        self.store_permission_sender(&params.sessionId, permission_sender).await;

        // Spawn blocking task for streaming with interactive mode (supports permissions)
        let streaming_handle = tokio::task::spawn_blocking(move || {
            let on_update = |notification: &str| -> Result<(), String> {
                sync_tx.send(notification.to_string())
                    .map_err(|e| format!("Failed to send: {}", e))
            };

            streaming_session.send_prompt_interactive_with_permission(&message_inner, on_update, perm_rx)
        });

        // Relay notifications from sync channel to async channel
        let tx_relay = tx.clone();
        let relay_handle = tokio::spawn(async move {
            while let Ok(notification) = sync_rx.recv() {
                if tx_relay.send(notification).await.is_err() {
                    break;
                }
            }
        });

        // Wait for streaming to complete
        match streaming_handle.await {
            Ok(result) => {
                // sync_tx is dropped when streaming_handle finishes, closing the channel
                let _ = relay_handle.await;

                // Clean up permission sender
                self.take_permission_sender(&session_id_inner).await;

                match result {
                    super::streaming::StreamingResult::Completed(_stop_reason, content) => {
                        AcpResponse::success(request.id, serde_json::json!({
                            "stopReason": "end_turn",
                            "response": content
                        }))
                    }
                    super::streaming::StreamingResult::PermissionNeeded { request: perm, response_rx, .. } => {
                        // Send permission notification before returning
                        let notification = AcpResponse::notification("requestPermission", serde_json::json!({
                            "requestId": perm.request_id,
                            "sessionId": session_id_inner,
                            "description": perm.description,
                            "toolCall": perm.tool_call.as_ref().map(|tc| serde_json::json!({
                                "toolCallId": tc.tool_call_id,
                                "title": tc.title,
                            })),
                            "options": perm.options.iter().enumerate().map(|(i, opt)| {
                                serde_json::json!({
                                    "optionId": i.to_string(),
                                    "label": opt.label,
                                    "kind": if i == 0 { "allow_once" } else if i == 1 { "allow_always" } else if i == 2 { "reject_once" } else { "reject_always" }
                                })
                            }).collect::<Vec<_>>()
                        }));

                        if let Ok(json) = notification.to_ndjson() {
                            let _ = tx.send(json).await;
                        }

                        AcpResponse::success(request.id, serde_json::json!({
                            "stopReason": "permission_required",
                            "permissionRequest": {
                                "requestId": perm.request_id,
                                "description": perm.description,
                                "options": perm.options.iter().enumerate().map(|(i, opt)| {
                                    serde_json::json!({
                                        "optionId": i.to_string(),
                                        "label": opt.label
                                    })
                                }).collect::<Vec<_>>()
                            }
                        }))
                    }
                    super::streaming::StreamingResult::Error(e) => {
                        AcpResponse::error(request.id, -32603, &format!("Streaming error: {}", e))
                    }
                }
            }
            Err(e) => {
                let _ = relay_handle.await;
                self.take_permission_sender(&session_id_inner).await;
                AcpResponse::error(request.id, -32603, &format!("Task error: {}", e))
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
        let _count = self.session_manager.session_count().await;

        AcpResponse::success(request.id, serde_json::json!({
            "sessions": [],
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

    /// Handle permissionResponse request (Phase 3)
    async fn handle_permission_response(&self, request: AcpRequest) -> AcpResponse {
        // Parse params - expecting { sessionId, outcome: { outcome: "selected", optionId } | { outcome: "cancelled" } }
        let params = match &request.params {
            Some(p) => p.clone(),
            None => {
                return AcpResponse::error(request.id, -32602, "Missing params");
            }
        };

        let session_id = params.get("sessionId")
            .and_then(|v| v.as_str())
            .map(String::from);

        let session_id = match session_id {
            Some(id) => id,
            None => {
                return AcpResponse::error(request.id, -32602, "Missing sessionId");
            }
        };

        let outcome = params.get("outcome");
        let outcome = match outcome {
            Some(o) => o.clone(),
            None => {
                return AcpResponse::error(request.id, -32602, "Missing outcome");
            }
        };

        // Parse outcome
        let outcome_type = outcome.get("outcome")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        match outcome_type {
            "selected" => {
                let option_id = outcome.get("optionId")
                    .and_then(|v| v.as_str())
                    .map(String::from);

                let option_id = match option_id {
                    Some(id) => id,
                    None => {
                        return AcpResponse::error(request.id, -32602, "Missing optionId for selected outcome");
                    }
                };

                // Parse option_id as choice index
                let choice: usize = option_id.parse().unwrap_or(1);

                tracing::info!("ACP permissionResponse: session={}, choice={}", session_id, choice);

                // Try to use streaming session first
                if let Some(perm_sender) = self.take_permission_sender(&session_id).await {
                    // Send choice to the streaming session
                    if perm_sender.tx.send(choice).is_ok() {
                        // The streaming session will continue and return the result
                        AcpResponse::success(request.id, serde_json::json!({
                            "stopReason": "end_turn"
                        }))
                    } else {
                        AcpResponse::error(request.id, -32603, "Failed to send permission choice to session")
                    }
                } else {
                    // Fall back to session manager's respond_permission
                    match self.session_manager.respond_permission(&session_id, choice).await {
                        Ok(result) => {
                            match result {
                                SendMessageResult::Response(response) => {
                                    AcpResponse::success(request.id, serde_json::json!({
                                        "stopReason": "end_turn",
                                        "response": response
                                    }))
                                }
                                SendMessageResult::PermissionNeeded(prompt) => {
                                    AcpResponse::success(request.id, serde_json::json!({
                                        "stopReason": "permission_required",
                                        "permissionRequest": {
                                            "requestId": prompt.request_id,
                                            "description": prompt.description,
                                            "options": prompt.options.iter().enumerate().map(|(i, opt)| {
                                                serde_json::json!({
                                                    "optionId": format!("opt_{}", i),
                                                    "label": opt.label
                                                })
                                            }).collect::<Vec<_>>()
                                        }
                                    }))
                                }
                                SendMessageResult::PermissionHandled { response, .. } => {
                                    AcpResponse::success(request.id, serde_json::json!({
                                        "stopReason": "end_turn",
                                        "response": response
                                    }))
                                }
                            }
                        }
                        Err(e) => {
                            AcpResponse::error(request.id, -32603, &format!("Failed to respond to permission: {}", e))
                        }
                    }
                }
            }
            "cancelled" => {
                tracing::info!("ACP permissionResponse: session={} cancelled", session_id);
                // Clean up any pending permission sender
                self.take_permission_sender(&session_id).await;
                AcpResponse::success(request.id, serde_json::json!({
                    "stopReason": "cancelled"
                }))
            }
            _ => {
                AcpResponse::error(request.id, -32602, &format!("Invalid outcome type: {}", outcome_type))
            }
        }
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

    /// Handle permission response with streaming support
    /// This is used when the initial prompt was streaming and needs to continue
    pub async fn handle_permission_response_streaming(
        &self,
        request: AcpRequest,
        tx: mpsc::Sender<String>,
    ) -> AcpResponse {
        // For now, delegate to the non-streaming version
        // The streaming session will be notified via the stored permission sender
        self.handle_permission_response(request).await
    }
}

/// Helper: write a line as NDJSON (with newline) and flush
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

                // Handle request with streaming support for prompt and permissionResponse
                if request.method == "prompt" || request.method == "permissionResponse" {
                    // Create channel for notifications
                    let (tx, rx) = mpsc::channel::<String>(32);

                    // Spawn task to write notifications
                    let writer_clone = writer.clone();
                    let notify_task = tokio::spawn(async move {
                        let mut rx = rx;
                        while let Some(notification) = rx.recv().await {
                            let mut w = writer_clone.lock().await;
                            if let Err(e) = write_ndjson(&mut *w, &notification).await {
                                tracing::error!("Failed to write notification: {}", e);
                                break;
                            }
                        }
                    });

                    // Handle with streaming
                    let response = if request.method == "prompt" {
                        handler.handle_prompt_streaming(request, tx).await
                    } else {
                        handler.handle_permission_response_streaming(request, tx).await
                    };

                    // Wait for notification task to complete (tx is dropped, so rx will end)
                    let _ = notify_task.await;

                    // Send final response
                    if let Ok(json) = response.to_ndjson() {
                        tracing::trace!("ACP response: {}", json);
                        let mut w = writer.lock().await;
                        let _ = write_ndjson(&mut *w, &json).await;
                    }
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
