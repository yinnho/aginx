//! Simplified handler for aginx
//!
//! Routes prompt requests to CLI agent processes.
//! No ACP handshake, no session management — just prompt → CLI → response.

use std::sync::Arc;

use tokio::sync::mpsc;

use crate::agent::AgentManager;
use crate::acp::adapter::PromptAdapter;
use crate::acp::types::*;

/// Handler for incoming requests
pub struct Handler {
    agent_manager: Arc<AgentManager>,
    access: crate::config::AccessMode,
    jwt_secret: Option<String>,
}

impl Handler {
    pub fn new(agent_manager: AgentManager) -> Self {
        Self {
            agent_manager: Arc::new(agent_manager),
            access: crate::config::AccessMode::default(),
            jwt_secret: None,
        }
    }

    pub fn with_access(access: crate::config::AccessMode, agent_manager: AgentManager) -> Self {
        Self {
            agent_manager: Arc::new(agent_manager),
            access,
            jwt_secret: None,
        }
    }

    pub fn with_jwt_secret(mut self, secret: Option<String>) -> Self {
        self.jwt_secret = secret;
        self
    }

    /// Check if the auth state allows the given method in the current access mode.
    /// In Private/Protected mode, unauthenticated connections can only call safe methods.
    fn is_allowed(&self, method: &str, auth: &crate::acp::ConnectionAuth) -> bool {
        if matches!(self.access, crate::config::AccessMode::Public) {
            return true;
        }
        if matches!(auth, crate::acp::ConnectionAuth::Authenticated) {
            return true;
        }
        // Non-Public + Pending: only allow safe methods
        matches!(method, "listAgents" | "agents/list" | "ping" | "initialize" | "bindDevice")
    }

    /// Verify an auth token (binding token or JWT).
    /// Returns Ok(()) on success.
    fn verify_auth_token(&self, token: &str) -> Result<(), String> {
        // Try binding token first
        let binding_arc = crate::binding::get_binding_manager();
        let mut binding_mgr = binding_arc
            .lock()
            .map_err(|e| format!("Binding manager lock failed: {}", e))?;
        if binding_mgr.verify_token(token).is_some() {
            return Ok(());
        }
        drop(binding_mgr);

        // Fall back to JWT
        if let Some(ref secret) = self.jwt_secret {
            if crate::auth::verify_jwt(token, secret).is_ok() {
                return Ok(());
            }
        }

        Err("Invalid or expired token".to_string())
    }

    /// Handle a non-streaming request
    pub async fn handle_request(
        &self,
        request: AcpRequest,
        auth: crate::acp::ConnectionAuth,
    ) -> (AcpResponse, Option<crate::acp::ConnectionAuth>) {
        if !self.is_allowed(&request.method, &auth) {
            return (
                AcpResponse::error(request.id, -32600, "Authentication required"),
                None,
            );
        }

        match request.method.as_str() {
            "initialize" => {
                let mut authenticated = false;
                // Extract authToken from _meta if present
                if let Some(ref params) = request.params {
                    if let Some(meta) = params.get("_meta") {
                        if let Some(token) = meta.get("authToken").and_then(|v| v.as_str()) {
                            authenticated = self.verify_auth_token(token).is_ok();
                        }
                    }
                }
                // Also check top-level token field for backward compatibility
                if !authenticated {
                    if let Some(ref params) = request.params {
                        if let Some(token) = params.get("token").and_then(|v| v.as_str()) {
                            authenticated = self.verify_auth_token(token).is_ok();
                        }
                    }
                }

                let new_auth = if authenticated {
                    crate::acp::ConnectionAuth::Authenticated
                } else {
                    crate::acp::ConnectionAuth::Pending
                };

                let response = AcpResponse::success(
                    request.id,
                    serde_json::json!({
                        "protocolVersion": 1,
                        "authenticated": authenticated,
                        "serverInfo": {
                            "name": "aginx",
                            "version": env!("CARGO_PKG_VERSION"),
                        }
                    }),
                );
                (response, Some(new_auth))
            }
            "bindDevice" => {
                let result = self.handle_bind_device(request).await;
                // bindDevice always returns the response; auth state change
                // happens only on success, caller checks the returned auth
                (result.0, result.1)
            }
            "listAgents" | "agents/list" => {
                let agents = self.agent_manager.list_agents().await;
                (
                    AcpResponse::success(request.id, serde_json::json!({"agents": agents})),
                    None,
                )
            }
            "ping" => (
                AcpResponse::success(request.id, serde_json::json!({"pong": true})),
                None,
            ),
            _ => (
                AcpResponse::error(request.id, -32601, &format!("Method not found: {}", request.method)),
                None,
            ),
        }
    }

    /// Handle bindDevice request
    async fn handle_bind_device(
        &self,
        request: AcpRequest,
    ) -> (AcpResponse, Option<crate::acp::ConnectionAuth>) {
        #[derive(serde::Deserialize)]
        struct BindParams {
            pair_code: String,
            device_name: String,
        }

        let params: BindParams = match request.params {
            Some(ref p) => match serde_json::from_value(p.clone()) {
                Ok(p) => p,
                Err(e) => {
                    return (
                        AcpResponse::error(request.id, -32602, &format!("Invalid params: {}", e)),
                        None,
                    );
                }
            },
            None => {
                return (
                    AcpResponse::error(request.id, -32602, "Missing params"),
                    None,
                );
            }
        };

        let binding_arc = crate::binding::get_binding_manager();
        let mut binding_mgr = match binding_arc.lock() {
            Ok(mgr) => mgr,
            Err(e) => {
                return (
                    AcpResponse::error(request.id, -32603, &format!("Internal error: {}", e)),
                    None,
                );
            }
        };

        match binding_mgr.bind_device(&params.pair_code, &params.device_name) {
            crate::binding::BindResult::Success(device) => {
                let response = AcpResponse::success(
                    request.id,
                    serde_json::json!({
                        "deviceId": device.id,
                        "deviceName": device.name,
                        "token": device.token,
                    }),
                );
                (response, Some(crate::acp::ConnectionAuth::Authenticated))
            }
            crate::binding::BindResult::AlreadyBound { device_name } => {
                let response = AcpResponse::error(
                    request.id,
                    -32600,
                    &format!("Already bound to device: {}", device_name),
                );
                (response, None)
            }
            crate::binding::BindResult::InvalidCode => {
                let response = AcpResponse::error(request.id, -32600, "Invalid or expired pair code");
                (response, None)
            }
        }
    }

    /// Handle a streaming prompt request
    pub async fn handle_prompt(
        &self,
        request: AcpRequest,
        tx: mpsc::Sender<String>,
        auth: crate::acp::ConnectionAuth,
    ) -> AcpResponse {
        if !self.is_allowed("prompt", &auth) {
            return AcpResponse::error(request.id, -32600, "Authentication required");
        }

        // Parse params
        let params: PromptParams = match request.params {
            Some(ref p) => match serde_json::from_value(p.clone()) {
                Ok(p) => p,
                Err(e) => return AcpResponse::error(request.id, -32602, &format!("Invalid params: {}", e)),
            },
            None => return AcpResponse::error(request.id, -32602, "Missing params"),
        };

        // Find agent
        let agent_info = match self.agent_manager.get_agent_info(&params.agent).await {
            Some(info) => info,
            None => return AcpResponse::error(request.id, -32601, &format!("Agent not found: {}", params.agent)),
        };

        // Create adapter and run prompt
        let adapter = PromptAdapter::new(&agent_info);
        let session_id = params.sessionId.clone().or_else(|| Some(crate::agent::new_session_id()));
        let session_id_ref = session_id.as_deref();

        adapter.prompt(&params.message, session_id_ref, params.cwd.as_deref(), tx).await;

        AcpResponse::success(request.id, serde_json::json!({
            "streaming": true,
            "sessionId": session_id,
        }))
    }
}
