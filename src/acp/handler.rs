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
    auto_approve: bool,
    jwt_secret: Option<String>,
}

/// Admin methods: Bound (owner device) only. Visitors holding Authorized
/// tokens can never call these — a client must not be able to approve itself.
const ADMIN_METHODS: &[&str] = &[
    "listRequests",
    "approveRequest",
    "rejectRequest",
    "listClients",
    "revokeClient",
];

impl Handler {
    pub fn new(agent_manager: AgentManager) -> Self {
        Self {
            agent_manager: Arc::new(agent_manager),
            access: crate::config::AccessMode::default(),
            auto_approve: false,
            jwt_secret: None,
        }
    }

    pub fn with_access(
        access: crate::config::AccessMode,
        auto_approve: bool,
        agent_manager: AgentManager,
    ) -> Self {
        Self {
            agent_manager: Arc::new(agent_manager),
            access,
            auto_approve,
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

        // No auth: only initialize, bindDevice and the consent-flow pair
        // (requestAccess/checkAccess — the visitor's entry door)
        let level = match auth {
            Some(l) => l,
            None => {
                return matches!(
                    method,
                    "initialize" | "bindDevice" | "requestAccess" | "checkAccess"
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
        // Admin methods are owner-only, regardless of claims (§2.9)
        if ADMIN_METHODS.contains(&method) {
            return false;
        }

        // Safe methods are always allowed
        if matches!(
            method,
            "listAgents" | "agents/list" | "sessions/list" | "ping" | "initialize"
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
            "requestAccess" => {
                let response = self.handle_request_access(request).await;
                (response, auth)
            }
            "checkAccess" => {
                let response = self.handle_check_access(request).await;
                (response, auth)
            }
            "listRequests" => self.owner_gate(request, &auth, |req| {
                let mgr = crate::auth::get_auth_manager();
                let m = mgr.lock().unwrap_or_else(|e| e.into_inner());
                AcpResponse::success(
                    req.id,
                    serde_json::json!({ "requests": m.list_requests() }),
                )
            }),
            "approveRequest" => {
                let response = self.handle_approve_request(request, &auth).await;
                (response, auth)
            }
            "rejectRequest" => self.owner_gate(request, &auth, |req| {
                #[derive(serde::Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct P { request_id: String }
                let p: P = match serde_json::from_value(req.params.clone().unwrap_or_default()) {
                    Ok(p) => p,
                    Err(_) => return AcpResponse::error(req.id, -32602, "Invalid params: requestId required"),
                };
                let mgr = crate::auth::get_auth_manager();
                let mut m = mgr.lock().unwrap_or_else(|e| e.into_inner());
                AcpResponse::success(req.id, serde_json::json!({ "removed": m.remove_request(&p.request_id) }))
            }),
            "listClients" => self.owner_gate(request, &auth, |req| {
                let mgr = crate::auth::get_auth_manager();
                let m = mgr.lock().unwrap_or_else(|e| e.into_inner());
                // Tokens never leave the gateway — the visitor fetch is
                // checkAccess-only, the owner never needs them
                let clients: Vec<serde_json::Value> = m.list_clients().into_iter().map(|c| {
                    serde_json::json!({
                        "id": c.id,
                        "name": c.name,
                        "createdAt": c.created_at,
                        "expiresAt": c.expires_at,
                        "allowedAgents": c.allowed_agents,
                        "allowSystem": c.allow_system,
                    })
                }).collect();
                AcpResponse::success(req.id, serde_json::json!({ "clients": clients }))
            }),
            "revokeClient" => self.owner_gate(request, &auth, |req| {
                #[derive(serde::Deserialize)]
                #[serde(rename_all = "camelCase")]
                struct P { client_id: String }
                let p: P = match serde_json::from_value(req.params.clone().unwrap_or_default()) {
                    Ok(p) => p,
                    Err(_) => return AcpResponse::error(req.id, -32602, "Invalid params: clientId required"),
                };
                let mgr = crate::auth::get_auth_manager();
                let mut m = mgr.lock().unwrap_or_else(|e| e.into_inner());
                AcpResponse::success(req.id, serde_json::json!({ "removed": m.remove_client(&p.client_id) }))
            }),
            "listAgents" | "agents/list" => {
                let agents = self.agent_manager.list_agents().await;
                // Filter agents based on authorization
                let filtered = match &auth {
                    Some(AuthLevel::Authorized(client)) if !client.allowed_agents.is_empty() => {
                        agents.into_iter()
                            .filter(|a| a.get("id")
                                .and_then(|v| v.as_str())
                                .map(|id| client.allowed_agents.contains(&id.to_string()))
                                .unwrap_or(false))
                            .collect()
                    }
                    _ => agents,
                };
                (
                    AcpResponse::success(request.id, serde_json::json!({"agents": filtered})),
                    auth,
                )
            }
            "sessions/list" => {
                // §2.4.1：{agent} → {sessions:[…]}。事实源 = 网关台账
                // （经手轮以收割的真 sessionId 记账；raw 方言无收割 → 空表）。
                let params = request.params.clone().unwrap_or_default();
                let agent_id = params.get("agent").and_then(|v| v.as_str()).unwrap_or("");
                if agent_id.is_empty() {
                    (
                        AcpResponse::error(request.id, -32602, "sessions/list requires 'agent'"),
                        auth,
                    )
                } else if self.agent_manager.get_agent_info(agent_id).await.is_none() {
                    (
                        AcpResponse::error(request.id, -32602, &format!("Unknown agent: {}", agent_id)),
                        auth,
                    )
                } else {
                    (
                        AcpResponse::success(
                            request.id,
                            serde_json::json!({ "sessions": self.agent_manager.ledger.list(agent_id) }),
                        ),
                        auth,
                    )
                }
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
        #[serde(rename_all = "camelCase")]
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
            Err(_) => {
                return (
                    AcpResponse::error(request.id, -32603, "Internal server error"),
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
            crate::binding::BindResult::AlreadyBound { device_name: _ } => {
                let response = AcpResponse::error(
                    request.id,
                    -32600,
                    "Device already bound",
                );
                (response, auth)
            }
            crate::binding::BindResult::InvalidCode => {
                let response = AcpResponse::error(request.id, -32600, "Invalid or expired pair code");
                (response, auth)
            }
        }
    }

    /// Owner-only gate for admin methods. is_allowed already rejects
    /// visitors; this is the in-arm enforcement (public mode's 伪 Bound
    /// passes — public means everything is open, consistent with §2.2).
    fn owner_gate(
        &self,
        request: AcpRequest,
        auth: &Option<AuthLevel>,
        f: impl FnOnce(AcpRequest) -> AcpResponse,
    ) -> (AcpResponse, Option<AuthLevel>) {
        if !matches!(auth, Some(AuthLevel::Bound)) {
            return (
                AcpResponse::error(request.id, -32600, "Owner device required"),
                auth.clone(),
            );
        }
        (f(request), auth.clone())
    }

    /// requestAccess（同意流访客入口，§2.9）：挂 pending 队列等主人点同意；
    /// 网关配 `auto_approve = true` 时立即发放 scoped token（客服码即扫即用）。
    async fn handle_request_access(&self, request: AcpRequest) -> AcpResponse {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct P {
            client_name: String,
            #[serde(default)]
            agent: Option<String>,
        }
        let p: P = match request.params {
            Some(ref v) => match serde_json::from_value(v.clone()) {
                Ok(p) => p,
                Err(e) => return AcpResponse::error(request.id, -32602, &format!("Invalid params: {}", e)),
            },
            None => return AcpResponse::error(request.id, -32602, "Missing params"),
        };
        let name = p.client_name.trim().chars().take(64).collect::<String>();
        if name.is_empty() {
            return AcpResponse::error(request.id, -32602, "clientName required");
        }
        let agent = p.agent.map(|a| a.trim().to_string()).filter(|a| !a.is_empty());

        let mgr = crate::auth::get_auth_manager();
        let mut m = mgr.lock().unwrap_or_else(|e| e.into_inner());

        if self.auto_approve {
            // Scoped to the 客服码 suffix when present; no suffix = all agents
            // (same surface public mode would expose, but revocable per client)
            let allowed = agent.clone().map(|a| vec![a]).unwrap_or_default();
            let client = m.issue_client(&name, allowed, Some(30));
            m.add_client(client.clone()).ok();
            tracing::info!(client = %client.id, agent = ?agent, "auto-approve: visitor token issued");
            return AcpResponse::success(request.id, serde_json::json!({
                "status": "approved",
                "clientId": client.id,
                "token": client.token,
                "allowedAgents": client.allowed_agents,
            }));
        }

        let req = m.add_request(&name, agent.as_deref());
        tracing::info!(request = %req.request_id, client = %name, "access request queued");
        AcpResponse::success(request.id, serde_json::json!({
            "status": "pending",
            "requestId": req.request_id,
        }))
    }

    /// checkAccess（访客轮询取票）：approved = 一次性发 token 并销单；
    /// pending = 主人还没处理；notFound = 被拒/已取过/过期（同一状态，不泄露原因）。
    async fn handle_check_access(&self, request: AcpRequest) -> AcpResponse {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct P { request_id: String }
        let p: P = match request.params {
            Some(ref v) => match serde_json::from_value(v.clone()) {
                Ok(p) => p,
                Err(_) => return AcpResponse::error(request.id, -32602, "Invalid params: requestId required"),
            },
            None => return AcpResponse::error(request.id, -32602, "Missing params"),
        };

        let mgr = crate::auth::get_auth_manager();
        let mut m = mgr.lock().unwrap_or_else(|e| e.into_inner());
        // take_request_token: approved→一次性取票销单；其余状态不销
        if let Some(token) = m.take_request_token(&p.request_id) {
            tracing::info!(request = %p.request_id, "access token handed to visitor");
            return AcpResponse::success(request.id, serde_json::json!({
                "status": "approved",
                "token": token,
            }));
        }
        let still_pending = m.list_requests().iter().any(|r| r.request_id == p.request_id);
        AcpResponse::success(request.id, serde_json::json!({
            "status": if still_pending { "pending" } else { "notFound" },
        }))
    }

    /// approveRequest（主人同意）：发 per-访客 AuthorizedClient（agent 范围
    /// 默认 = 申请时的客服码后缀），token 记在 pending 单上等访客来取。
    async fn handle_approve_request(&self, request: AcpRequest, auth: &Option<AuthLevel>) -> AcpResponse {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct P {
            request_id: String,
            #[serde(default)]
            allowed_agents: Option<Vec<String>>,
            #[serde(default)]
            expire_days: Option<i64>,
        }
        if !matches!(auth, Some(AuthLevel::Bound)) {
            return AcpResponse::error(request.id, -32600, "Owner device required");
        }
        let p: P = match request.params {
            Some(ref v) => match serde_json::from_value(v.clone()) {
                Ok(p) => p,
                Err(e) => return AcpResponse::error(request.id, -32602, &format!("Invalid params: {}", e)),
            },
            None => return AcpResponse::error(request.id, -32602, "Missing params"),
        };

        let mgr = crate::auth::get_auth_manager();
        let mut m = mgr.lock().unwrap_or_else(|e| e.into_inner());

        let Some(req) = m.list_requests().into_iter().find(|r| r.request_id == p.request_id) else {
            return AcpResponse::error(request.id, -32602, &format!("Request not found: {}", p.request_id));
        };
        // Scope: explicit param > the 客服码 suffix the visitor asked for > all agents
        let allowed = p.allowed_agents
            .filter(|v| !v.is_empty())
            .or_else(|| req.agent.clone().map(|a| vec![a]))
            .unwrap_or_default();
        let client = m.issue_client(&req.client_name, allowed.clone(), p.expire_days);
        m.add_client(client.clone()).ok();
        m.set_request_token(&p.request_id, &client.token);
        tracing::info!(request = %p.request_id, client = %client.id, agents = ?allowed, "access request approved");
        AcpResponse::success(request.id, serde_json::json!({
            "client": {
                "id": client.id,
                "name": client.name,
                "allowedAgents": client.allowed_agents,
                "expiresAt": client.expires_at,
            }
        }))
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
        tracing::info!(
            agent = %params.agent,
            has_ticket = params.sessionTicket.is_some(),
            borrower = ?params.borrower,
            "prompt dispatch"
        );

        // Find agent
        let agent_info = match self.agent_manager.get_agent_info(&params.agent).await {
            Some(info) => info,
            None => return AcpResponse::error(request.id, -32601, &format!("Agent not found: {}", params.agent)),
        };

        // Create adapter and run prompt
        let adapter = PromptAdapter::new(&agent_info, self.agent_manager.ledger.clone());
        // sessionId 只采信 client 显式传入——不再自动生成：
        // 生成的 uuid 会被 PromptAdapter 拼进 resume_args（如 `--resume <uuid>`），
        // headless CLI（claude/copilot）不认识该 id，首轮即 "No conversation found" 必炸。
        let session_id = params.sessionId.clone();
        // Validate sessionId: only allow alphanumeric, hyphens, underscores
        if let Some(ref sid) = session_id {
            if !sid.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_') {
                return AcpResponse::error(request.id, -32602, "Invalid sessionId: only alphanumeric, hyphens, underscores allowed");
            }
        }
        let session_id_ref = session_id.as_deref();

        // 借用轮直通：prompt 带 sessionTicket → 一次性 ACP 会话（票据进/出 +
        // 素材进 + 产物 files 回），主人服务器零持久化。最终 result 经 tx 发出
        // （含 sessionTicket/files），不再走下方固定 success 响应。
        if let Some(session_ticket) = params.sessionTicket.clone() {
            // 鉴权身份透传给桥做准入/配额。优先级：
            // - Authorized：用 client.id（真实身份，不可冒充）
            // - Bound：主人本人设备，不透传（桥按 local 处理，不受名单门限）
            // - public 模式的"伪 Bound"（无鉴权连接被置为 Bound）：采信客户端
            //   显式 borrower——public 本就无门，friends 名单门交给桥侧 [borrow]
            //   配置；private/protected 不会出现此分支。
            let borrower = match &auth {
                Some(AuthLevel::Authorized(client)) => Some(client.id.clone()),
                Some(AuthLevel::Bound) => {
                    if matches!(self.access, crate::config::AccessMode::Public) {
                        params.borrower.clone()
                    } else {
                        None
                    }
                }
                None => params.borrower.clone(),
            };
            let _ = adapter
                .prompt_borrowed(
                    &params.message,
                    session_ticket,
                    params.materials.clone(),
                    params.activeFlow.clone(),
                    borrower,
                    params.cwd.as_deref(),
                    tx,
                )
                .await;
            tracing::info!("borrowed passthrough returned");
            return AcpResponse::success(request.id, serde_json::json!({
                "streaming": true,
                "sessionId": session_id,
            }));
        }

        adapter.prompt(&params.message, session_id_ref, params.cwd.as_deref(), tx).await;

        AcpResponse::success(request.id, serde_json::json!({
            "streaming": true,
            "sessionId": session_id,
        }))
    }
}
