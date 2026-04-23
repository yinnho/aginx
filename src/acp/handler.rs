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
}

impl Handler {
    pub fn new(agent_manager: AgentManager) -> Self {
        Self {
            agent_manager: Arc::new(agent_manager),
            access: crate::config::AccessMode::default(),
        }
    }

    pub fn with_access(access: crate::config::AccessMode, agent_manager: AgentManager) -> Self {
        Self {
            agent_manager: Arc::new(agent_manager),
            access,
        }
    }

    /// Check if the auth state allows the given method in the current access mode.
    /// In Private mode, unauthenticated connections can only call safe methods.
    fn is_allowed(&self, method: &str, auth: &crate::acp::ConnectionAuth) -> bool {
        if matches!(self.access, crate::config::AccessMode::Public) {
            return true;
        }
        if matches!(auth, crate::acp::ConnectionAuth::Authenticated) {
            return true;
        }
        // Private + Pending: only allow safe methods
        matches!(method, "listAgents" | "agents/list" | "ping" | "initialize" | "bindDevice")
    }

    /// Handle a non-streaming request
    pub async fn handle_request(&self, request: AcpRequest, auth: crate::acp::ConnectionAuth) -> AcpResponse {
        if !self.is_allowed(&request.method, &auth) {
            return AcpResponse::error(request.id, -32600, "Authentication required");
        }

        match request.method.as_str() {
            "listAgents" | "agents/list" => {
                let agents = self.agent_manager.list_agents().await;
                AcpResponse::success(request.id, serde_json::json!({"agents": agents}))
            }
            "ping" => {
                AcpResponse::success(request.id, serde_json::json!({"pong": true}))
            }
            _ => {
                AcpResponse::error(request.id, -32601, &format!("Method not found: {}", request.method))
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
