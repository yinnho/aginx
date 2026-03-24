//! ACP Handler implementation
//!
//! Handles ACP protocol requests and routes them to appropriate handlers.

use std::sync::Arc;

use tokio::sync::{Mutex, mpsc};

use super::types::{AcpRequest, AcpResponse, InitializeParams, InitializeResult, NewSessionParams, LoadSessionParams, PromptParams, ContentBlock, PermissionOutcome};
use super::streaming::StreamingSession;
use crate::agent::{AgentManager, SessionManager, SessionInfo, SendMessageResult};

/// ACP Handler that processes ACP protocol messages
pub struct AcpHandler {
    /// Agent manager for getting agent info
    agent_manager: Arc<AgentManager>,
    /// Session manager for creating/managing sessions
    session_manager: Arc<SessionManager>,
    /// Initialize result (cached after first initialize)
    initialized: Mutex<bool>,
}

impl AcpHandler {
    /// Create a new ACP handler
    pub fn new(agent_manager: Arc<AgentManager>, session_manager: Arc<SessionManager>) -> Self {
        Self {
            agent_manager,
            session_manager,
            initialized: Mutex::new(false),
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
            "permissionResponse" => self.handle_permission_response(request).await,
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

        // Get agent ID from _meta or use default
        let agent_id = params._meta
            .as_ref()
            .and_then(|m| m.get("agentId"))
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| "claude".to_string());

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

        // For now, just return success with the session ID
        // TODO: Implement actual session loading/resumption
        tracing::info!("ACP loadSession: {}", params.sessionId);

        AcpResponse::success(request.id, serde_json::json!({
            "sessionId": params.sessionId
        }))
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
            SendMessageResult::Response(_response) => {
                // Return success with end_turn
                AcpResponse::success(request.id, serde_json::json!({
                    "stopReason": "end_turn"
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

        // Get session info to create streaming session
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

        // Create streaming session
        let mut streaming_session = StreamingSession::new(agent_info, session_info.workdir.as_deref());

        // For true streaming, we use spawn_blocking because StreamingSession uses blocking I/O
        // We use a sync channel within the blocking task and relay to async channel
        let (sync_tx, sync_rx) = std::sync::mpsc::channel::<String>();
        let message_inner = message.clone();

        // Spawn blocking task for streaming
        // sync_tx is moved into the closure and will be dropped when streaming finishes,
        // which will close the channel and end the relay
        let streaming_handle = tokio::task::spawn_blocking(move || {
            let on_update = |notification: &str| -> Result<(), String> {
                sync_tx.send(notification.to_string())
                    .map_err(|e| format!("Failed to send: {}", e))
            };

            streaming_session.send_prompt_streaming(&message_inner, on_update)
        });

        // Relay notifications from sync channel to async channel
        let relay_handle = tokio::spawn(async move {
            while let Ok(notification) = sync_rx.recv() {
                if tx.send(notification).await.is_err() {
                    break;
                }
            }
        });

        // Wait for streaming to complete
        match streaming_handle.await {
            Ok(Ok(_stop_reason)) => {
                // sync_tx is dropped when streaming_handle finishes, closing the channel
                let _ = relay_handle.await;

                AcpResponse::success(request.id, serde_json::json!({
                    "stopReason": "end_turn"
                }))
            }
            Ok(Err(e)) => {
                let _ = relay_handle.await;
                AcpResponse::error(request.id, -32603, &format!("Streaming error: {}", e))
            }
            Err(e) => {
                let _ = relay_handle.await;
                AcpResponse::error(request.id, -32603, &format!("Task error: {}", e))
            }
        }
    }

    /// Handle cancel request
    async fn handle_cancel(&self, request: AcpRequest) -> AcpResponse {
        tracing::info!("ACP cancel requested");
        // TODO: Implement actual cancellation
        AcpResponse::success(request.id, serde_json::json!({
            "cancelled": true
        }))
    }

    /// Handle listSessions request
    async fn handle_list_sessions(&self, request: AcpRequest) -> AcpResponse {
        let _count = self.session_manager.session_count().await;

        AcpResponse::success(request.id, serde_json::json!({
            "sessions": [],
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

                // Parse option_id as choice index (e.g., "opt_0" -> 0)
                let choice: usize = if option_id.starts_with("opt_") {
                    option_id[4..].parse().unwrap_or(0)
                } else {
                    option_id.parse().unwrap_or(0)
                };

                tracing::info!("ACP permissionResponse: session={}, choice={}", session_id, choice);

                // Call session manager to respond to permission
                match self.session_manager.respond_permission(&session_id, choice).await {
                    Ok(result) => {
                        match result {
                            SendMessageResult::Response(response) => {
                                // Permission handled, got final response
                                AcpResponse::success(request.id, serde_json::json!({
                                    "stopReason": "end_turn",
                                    "response": response
                                }))
                            }
                            SendMessageResult::PermissionNeeded(prompt) => {
                                // Another permission needed - return as notification request
                                // Client should handle this by showing another permission dialog
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
            "cancelled" => {
                tracing::info!("ACP permissionResponse: session={} cancelled", session_id);
                AcpResponse::success(request.id, serde_json::json!({
                    "stopReason": "cancelled"
                }))
            }
            _ => {
                AcpResponse::error(request.id, -32602, &format!("Invalid outcome type: {}", outcome_type))
            }
        }
    }
}

/// Run ACP protocol over stdin/stdout with streaming support
pub async fn run_acp_stdio(agent_manager: Arc<AgentManager>, session_manager: Arc<SessionManager>) {
    let handler = Arc::new(AcpHandler::new(agent_manager, session_manager));

    tracing::info!("Starting ACP stdio mode");

    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

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
                            let _ = w.write_all(json.as_bytes()).await;
                            let _ = w.write_all(b"\n").await;
                            let _ = w.flush().await;
                        }
                        continue;
                    }
                };

                // Handle request with streaming support for prompt
                if request.method == "prompt" {
                    // Create channel for notifications
                    let (tx, rx) = mpsc::channel::<String>(32);

                    // Spawn task to write notifications
                    let writer_clone = writer.clone();
                    let notify_task = tokio::spawn(async move {
                        let mut rx = rx;
                        while let Some(notification) = rx.recv().await {
                            let mut w = writer_clone.lock().await;
                            if let Err(e) = w.write_all(notification.as_bytes()).await {
                                tracing::error!("Failed to write notification: {}", e);
                                break;
                            }
                            if let Err(e) = w.write_all(b"\n").await {
                                tracing::error!("Failed to write newline: {}", e);
                                break;
                            }
                            if let Err(e) = w.flush().await {
                                tracing::error!("Failed to flush: {}", e);
                                break;
                            }
                        }
                    });

                    // Handle with streaming
                    let response = handler.handle_prompt_streaming(request, tx).await;

                    // Wait for notification task to complete (tx is dropped, so rx will end)
                    let _ = notify_task.await;

                    // Send final response
                    if let Ok(json) = response.to_ndjson() {
                        tracing::trace!("ACP response: {}", json);
                        let mut w = writer.lock().await;
                        let _ = w.write_all(json.as_bytes()).await;
                        let _ = w.write_all(b"\n").await;
                        let _ = w.flush().await;
                    }
                } else {
                    // Non-streaming methods
                    let response = handler.handle_request(request).await;

                    // Send response
                    if let Ok(json) = response.to_ndjson() {
                        tracing::trace!("ACP response: {}", json);
                        let mut w = writer.lock().await;
                        let _ = w.write_all(json.as_bytes()).await;
                        let _ = w.write_all(b"\n").await;
                        let _ = w.flush().await;
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
