//! Simplified handler for aginx
//!
//! Routes prompt requests to CLI agent processes.
//! No ACP handshake, no session management — just prompt → CLI → response.

use std::sync::Arc;

use tokio::sync::mpsc;

use crate::agent::AgentManager;
use crate::acp::adapter::PromptAdapter;
use crate::acp::types::*;
use crate::auth::{AuthLevel, AuthorizedClient};

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

    /// Check if the auth level allows the given method for the given agent.
    /// Public mode: always allowed.
    /// Bound: always allowed (full permissions).
    /// Authorized: restricted by JWT claims.
    /// Pending (no auth): only safe methods.
    fn is_allowed(
        &self,
        method: &str,
        agent_id: Option<&str>,
        auth: &Option<AuthLevel>,
    ) -> bool {
        // Public mode: no restrictions
        if matches!(self.access, crate::config::AccessMode::Public) {
            return true;
        }

        // No auth: only initialize and bindDevice
        let level = match auth {
            Some(l) => l,
            None => {
                return matches!(
                    method,
                    "initialize" | "bindDevice"
                );
            }
        };

        match level {
            AuthLevel::Bound => true,
            AuthLevel::Authorized(client) => {
                self.is_authorized_allowed(method, agent_id, client)
            }
        }
    }

    /// Check if an authorized client is allowed to call a method.
    fn is_authorized_allowed(
        &self,
        method: &str,
        agent_id: Option<&str>,
        client: &AuthorizedClient,
    ) -> bool {
        // Safe methods are always allowed
        if matches!(
            method,
            "listAgents" | "agents/list" | "ping" | "initialize"
        ) {
            return true;
        }

        // Check method whitelist
        if !client.allowed_methods.is_empty()
            && !client.allowed_methods.contains(&method.to_string())
        {
            return false;
        }

        // Check system methods
        let is_system = matches!(
            method,
            "listDirectory" | "readFile" | "bindDevice"
        );
        if is_system && !client.allow_system {
            return false;
        }

        // Check agent whitelist for prompt
        if method == "prompt" {
            if let Some(id) = agent_id {
                if !client.allowed_agents.is_empty()
                    && !client.allowed_agents.contains(&id.to_string())
                {
                    return false;
                }
            }
        }

        true
    }

    /// Verify an auth token.
    /// Returns Some(AuthLevel) on success, None on failure.
    fn verify_auth_token(&self, token: &str) -> Option<AuthLevel> {
        // Try binding token first (full permissions)
        let binding_arc = crate::binding::get_binding_manager();
        let mut binding_mgr = binding_arc.lock().ok()?;
        if binding_mgr.verify_token(token).is_some() {
            return Some(AuthLevel::Bound);
        }
        drop(binding_mgr);

        // Try authorized client token (restricted permissions)
        let auth_arc = crate::auth::get_auth_manager();
        let auth_mgr = auth_arc.lock().ok()?;
        if let Some(client) = auth_mgr.find_by_token(token) {
            // Check expiration
            if let Some(exp) = client.expires_at {
                let now = chrono::Utc::now().timestamp();
                if now >= exp {
                    return None;
                }
            }
            return Some(AuthLevel::Authorized(client.clone()));
        }
        drop(auth_mgr);

        // Fall back to JWT
        if let Some(ref secret) = self.jwt_secret {
            if let Ok(claims) = crate::auth::verify_auth_client_jwt(token, secret) {
                let client = AuthorizedClient {
                    id: claims.sub.clone(),
                    name: claims.name,
                    token: token.to_string(),
                    created_at: claims.iat,
                    expires_at: Some(claims.exp),
                    allowed_agents: claims.agents,
                    allowed_methods: claims.methods,
                    allow_system: claims.sys,
                };
                return Some(AuthLevel::Authorized(client));
            }
        }

        None
    }

    /// Handle a non-streaming request
    pub async fn handle_request(
        &self,
        request: AcpRequest,
        auth: Option<AuthLevel>,
    ) -> (AcpResponse, Option<AuthLevel>) {
        let agent_id = request.params.as_ref()
            .and_then(|p| p.get("agent"))
            .and_then(|v| v.as_str());

        if !self.is_allowed(&request.method, agent_id, &auth) {
            return (
                AcpResponse::error(request.id, -32600, "Authentication required"),
                auth,
            );
        }

        match request.method.as_str() {
            "initialize" => {
                let mut new_auth = auth.clone();
                // Extract authToken from _meta if present
                if let Some(ref params) = request.params {
                    if let Some(meta) = params.get("_meta") {
                        if let Some(token) = meta.get("authToken").and_then(|v| v.as_str()) {
                            new_auth = self.verify_auth_token(token);
                        }
                    }
                }
                // Also check top-level token field for backward compatibility
                if new_auth.is_none() {
                    if let Some(ref params) = request.params {
                        if let Some(token) = params.get("token").and_then(|v| v.as_str()) {
                            new_auth = self.verify_auth_token(token);
                        }
                    }
                }

                let authenticated = new_auth.is_some();
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
                (response, new_auth)
            }
            "bindDevice" => {
                let (response, new_auth) = self.handle_bind_device(request, auth).await;
                (response, new_auth)
            }
            "listAgents" | "agents/list" => {
                let agents = self.agent_manager.list_agents().await;
                (
                    AcpResponse::success(request.id, serde_json::json!({"agents": agents})),
                    auth,
                )
            }
            "ping" => (
                AcpResponse::success(request.id, serde_json::json!({"pong": true})),
                auth,
            ),
            _ => (
                AcpResponse::error(request.id, -32601, &format!("Method not found: {}", request.method)),
                auth,
            ),
        }
    }

    /// Handle bindDevice request
    async fn handle_bind_device(
        &self,
        request: AcpRequest,
        auth: Option<AuthLevel>,
    ) -> (AcpResponse, Option<AuthLevel>) {
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
                        auth,
                    );
                }
            },
            None => {
                return (
                    AcpResponse::error(request.id, -32602, "Missing params"),
                    auth,
                );
            }
        };

        let binding_arc = crate::binding::get_binding_manager();
        let mut binding_mgr = match binding_arc.lock() {
            Ok(mgr) => mgr,
            Err(e) => {
                return (
                    AcpResponse::error(request.id, -32603, &format!("Internal error: {}", e)),
                    auth,
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
                (response, Some(AuthLevel::Bound))
            }
            crate::binding::BindResult::AlreadyBound { device_name } => {
                let response = AcpResponse::error(
                    request.id,
                    -32600,
                    &format!("Already bound to device: {}", device_name),
                );
                (response, auth)
            }
            crate::binding::BindResult::InvalidCode => {
                let response = AcpResponse::error(request.id, -32600, "Invalid or expired pair code");
                (response, auth)
            }
        }
    }

    /// Handle a streaming prompt request
    pub async fn handle_prompt(
        &self,
        request: AcpRequest,
        tx: mpsc::Sender<String>,
        auth: Option<AuthLevel>,
    ) -> AcpResponse {
        let agent_id = request.params.as_ref()
            .and_then(|p| p.get("agent"))
            .and_then(|v| v.as_str());

        if !self.is_allowed("prompt", agent_id, &auth) {
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
