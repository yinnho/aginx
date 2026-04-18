//! ACP Handler implementation
//!
//! Handles ACP protocol requests and routes them to appropriate handlers.

use std::sync::Arc;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::Duration;

use tokio::sync::mpsc;

use super::types::{AcpRequest, AcpResponse, InitializeParams, InitializeResult, NewSessionResult, PromptParams, ContentBlock};
use super::ConnectionAuth;
use super::notifications::write_ndjson;
use super::adapter;
use crate::agent::{AgentInfo, AgentManager, SessionManager};
use crate::binding::{self, BindResult};

/// ACP Handler that processes ACP protocol messages
pub struct AcpHandler {
    /// Agent manager for getting agent info
    agent_manager: Arc<AgentManager>,
    /// Session manager for creating/managing sessions
    session_manager: Arc<SessionManager>,
    /// Adapter cache for persistent process reuse
    adapter_cache: Arc<tokio::sync::RwLock<HashMap<String, Arc<dyn adapter::PromptAdapter>>>>,
    /// Default agents directory for discovery
    agents_dir: std::path::PathBuf,
    /// Global default access mode (from config)
    global_access: crate::config::AccessMode,
    /// JWT secret for aginx-to-aginx authentication
    jwt_secret: Option<String>,
    /// API URL for remote agent discovery
    api_url: Option<String>,
    /// JWT token for aginx-to-aginx authentication (from relay registration)
    aginx_token: Option<String>,
    /// Relay domain for remote/listAgents relay address construction
    relay_domain: Option<String>,
    /// Relay port
    relay_port: u16,
    /// Permission router for agent permission request/response flow
    permission_router: Arc<crate::acp::permission::PermissionRouter>,
    /// E2EE session for relay encryption (created lazily in relay mode)
    e2ee: Arc<crate::relay::e2ee::E2eeSession>,
    /// Map from ACP session ID to agent ID (for routing prompts to the right adapter)
    acp_session_map: Arc<Mutex<HashMap<String, String>>>,
}

impl AcpHandler {
    /// Create a new ACP handler
    pub fn new(agent_manager: Arc<AgentManager>, session_manager: Arc<SessionManager>) -> Self {
        Self {
            agent_manager,
            session_manager,
            adapter_cache: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            agents_dir: crate::config::agents_dir(),
            global_access: crate::config::AccessMode::default(),
            jwt_secret: None,
            api_url: None,
            aginx_token: None,
            relay_domain: None,
            relay_port: 8443,
            permission_router: Arc::new(crate::acp::permission::PermissionRouter::new()),
            e2ee: Arc::new(crate::relay::e2ee::E2eeSession::new()),
            acp_session_map: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Create ACP handler with global access mode
    pub fn with_access(agent_manager: Arc<AgentManager>, session_manager: Arc<SessionManager>, agents_dir: std::path::PathBuf, global_access: crate::config::AccessMode) -> Self {
        Self {
            agent_manager,
            session_manager,
            adapter_cache: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            agents_dir,
            global_access,
            jwt_secret: None,
            api_url: None,
            aginx_token: None,
            relay_domain: None,
            relay_port: 8443,
            permission_router: Arc::new(crate::acp::permission::PermissionRouter::new()),
            e2ee: Arc::new(crate::relay::e2ee::E2eeSession::new()),
            acp_session_map: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Get or create a cached adapter for the given agent.
    async fn get_adapter(&self, agent_info: &AgentInfo) -> Arc<dyn adapter::PromptAdapter> {
        {
            let cache = self.adapter_cache.read().await;
            if let Some(adapter) = cache.get(&agent_info.id) {
                return adapter.clone();
            }
        }
        let adapter = adapter::create_adapter(agent_info, self.session_manager.clone());
        let mut cache = self.adapter_cache.write().await;
        cache.insert(agent_info.id.clone(), adapter.clone());
        adapter
    }

    /// Set JWT secret for aginx-to-aginx authentication
    pub fn with_jwt_secret(mut self, secret: Option<String>) -> Self {
        self.jwt_secret = secret;
        self
    }

    /// Get E2EE public key as base64 string (for bindDevice response)
    pub fn e2ee_public_key(&self) -> String {
        self.e2ee.public_key_b64()
    }

    /// Set API URL for remote agent discovery
    pub fn with_api_url(mut self, url: Option<String>) -> Self {
        self.api_url = url;
        self
    }

    /// Set aginx JWT token for aginx-to-aginx calls
    pub fn with_aginx_token(mut self, token: Option<String>) -> Self {
        self.aginx_token = token;
        self
    }

    /// Set relay config for remote calls
    pub fn with_relay_config(mut self, domain: String, port: u16) -> Self {
        self.relay_domain = Some(domain);
        self.relay_port = port;
        self
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
            "initialize" => {
                let resp = self.handle_initialize(request).await;
                // Upgrade auth if initialize succeeded with valid token
                let upgrade = resp.result.as_ref()
                    .and_then(|r| r.get("authenticated"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                return (resp, upgrade);
            }
            "_aginx/bindDevice" => {
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

        // All other methods require authentication (closed trusted network)
        if auth == ConnectionAuth::Pending {
            return (AcpResponse::error(request.id, -32001, "Authentication required"), false);
        }

        match method.as_str() {
            "_aginx/listAgents" | "listAgents" => (self.handle_list_agents(request).await, false),
            "session/new" | "newSession" => (self.handle_new_session(request).await, false),
            "session/load" | "loadSession" => (self.handle_load_session(request).await, false),
            "session/cancel" => (self.handle_cancel(request).await, false),
            "session/list" => (self.handle_list_sessions(request).await, false),
            "session/set_mode" => (self.handle_set_mode(request).await, false),
            "session/prompt" => {
                (AcpResponse::error(request.id, -32601, "Use streaming endpoint for session/prompt"), false)
            }
            _ => (self.handle_request_internal(request).await, false)
        }
    }

    /// Internal handler for methods that require auth (already checked)
    async fn handle_request_internal(&self, request: AcpRequest) -> AcpResponse {
        match request.method.as_str() {
            "_aginx/listConversations" | "listConversations" => self.handle_list_conversations(request).await,
            "_aginx/deleteConversation" | "deleteConversation" => self.handle_delete_conversation(request).await,
            "_aginx/getMessages" | "getMessages" | "getConversationMessages" => self.handle_get_conversation_messages(request).await,
            "_aginx/listWorkspaces" | "listWorkspaces" => self.handle_list_workspaces(request).await,
            "_aginx/discoverAgents" | "discoverAgents" => self.handle_discover_agents(request).await,
            "_aginx/registerAgent" | "registerAgent" => self.handle_register_agent(request).await,
            "_aginx/discoverRemote" | "discoverRemote" => self.handle_remote_list_agents(request).await,
            "_aginx/remote/listAgents" | "remote/listAgents" => self.handle_remote_list_agents(request).await,
            "_aginx/listSessions" | "listSessions" => self.handle_list_sessions(request).await,
            "authenticate" => AcpResponse::success(request.id, serde_json::json!({})),
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
        let params: InitializeParams = match request.parse_params() {
            Ok(p) => p,
            Err(resp) => return resp,
        };

        // Check authToken from _meta — try namespaced key first, then legacy key
        // Also fallback to top-level params.authToken for relay compatibility
        let token = params._meta
            .as_ref()
            .and_then(|m| m.get("aginx/authToken").or(m.get("authToken")))
            .and_then(|v| v.as_str())
            .or_else(|| {
                request.params.as_ref()
                    .and_then(|p| p.get("authToken"))
                    .and_then(|v| v.as_str())
            });

        tracing::info!("[INIT] token present: {}, _meta: {:?}", token.is_some(), params._meta);

        let authenticated = if let Some(token) = token {
            // Path 1: local device binding token
            let mgr = crate::binding::get_binding_manager();
            let mut mgr = mgr.lock().unwrap();
            tracing::info!("[INIT] verifying token: {}...", &token[..token.len().min(20)]);
            if mgr.verify_token(token).is_some() {
                true
            } else {
                // Path 2: JWT token (aginx-to-aginx authentication)
                if let Some(ref secret) = self.jwt_secret {
                    crate::auth::verify_jwt(token, secret).map(|_id| true).unwrap_or_else(|e| {
                        tracing::warn!("JWT verification failed: {}", e);
                        false
                    })
                } else {
                    tracing::warn!("JWT auth attempted but no jwt_secret configured");
                    false
                }
            }
        } else {
            false
        };

        let client_name = params.clientInfo.as_ref().map(|i| i.name.as_str()).unwrap_or("unknown");
        let client_version = params.clientInfo.as_ref().map(|i| i.version.as_str()).unwrap_or("unknown");

        if authenticated {
            tracing::info!("ACP initialize from {} v{} (authenticated)", client_name, client_version);
        } else {
            tracing::info!("ACP initialize from {} v{} (unauthenticated)", client_name, client_version);
        }

        let result = InitializeResult::new();
        AcpResponse::success(request.id, serde_json::json!({
            "protocolVersion": result.protocolVersion,
            "agentCapabilities": result.agentCapabilities,
            "agentInfo": result.agentInfo,
            "authMethods": result.authMethods,
            "authenticated": authenticated,
        }))
    }

    /// Handle newSession request — forward to ACP agent, fallback to session_manager for Claude
    async fn handle_new_session(&self, request: AcpRequest) -> AcpResponse {
        let params = request.params_value();

        // Get agent ID from _meta (namespaced key)
        let agent_id = params.get("_meta")
            .and_then(|m| m.get("aginx/agentId").or(m.get("agentId")))
            .and_then(|v| v.as_str())
            .map(String::from)
            .or_else(|| params.get("agentId").and_then(|v| v.as_str()).map(String::from));

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

        let adapter = self.get_adapter(&agent_info).await;

        // Build clean ACP params — only standard session/new fields
        let mut acp_params = serde_json::json!({});
        // cwd: from client params, or default to $HOME
        if let Some(cwd) = params.get("cwd") {
            acp_params["cwd"] = cwd.clone();
        } else {
            acp_params["cwd"] = serde_json::Value::String(
                dirs::home_dir().map(|h| h.to_string_lossy().to_string()).unwrap_or("/".to_string())
            );
        }
        // mcpServers: some agents (Copilot) require this field
        if let Some(mcpServers) = params.get("mcpServers") {
            acp_params["mcpServers"] = mcpServers.clone();
        } else {
            acp_params["mcpServers"] = serde_json::json!([]);
        }

        // Forward to ACP agent — transparent passthrough with clean params
        match adapter.send_request("session/new", acp_params).await {
            Ok(mut result) => {
                tracing::info!("ACP newSession: forwarded to agent {}", agent_id);
                // Inject agentId into response so client knows which agent owns this session
                if let Some(obj) = result.as_object_mut() {
                    obj.insert("agentId".to_string(), serde_json::Value::String(agent_id.clone()));
                }
                // Register ACP session → agent_id mapping for prompt routing
                if let Some(sid) = result.get("sessionId").and_then(|v| v.as_str()) {
                    self.acp_session_map.lock().unwrap().insert(sid.to_string(), agent_id.clone());
                }
                return AcpResponse::success(request.id, result);
            }
            Err(e) => {
                // Only fallback to local session for Claude protocol
                if agent_info.protocol != "claude-stream" {
                    tracing::warn!("ACP session/new failed for agent {}: {}", agent_id, e);
                    return AcpResponse::error(request.id, -32603, &format!("Agent session/new failed: {}", e));
                }
            }
        }

        // Claude fallback: create session in session_manager
        let cwd = params.get("cwd").and_then(|v| v.as_str());
        let session_id = match self.session_manager.create_session(&agent_info, cwd).await {
            Ok(id) => id,
            Err(e) => {
                return AcpResponse::error(request.id, -32603, &format!("Failed to create session: {}", e));
            }
        };
        tracing::info!("ACP newSession (local): {} -> {}", session_id, agent_info.agent_type);

        AcpResponse::success(request.id, NewSessionResult {
            sessionId: session_id,
            modes: None,
            configOptions: None,
        })
    }

    /// Handle loadSession request — forward to ACP agent, fallback to session_manager for Claude
    async fn handle_load_session(&self, request: AcpRequest) -> AcpResponse {
        let params = request.params_value();

        let session_id = match params.get("sessionId").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => return AcpResponse::error(request.id, -32602, "Missing sessionId"),
        };

        // Get agentId from params or _meta
        let agent_id = params.get("agentId")
            .and_then(|v| v.as_str())
            .map(String::from)
            .or_else(|| {
                params.get("_meta")
                    .and_then(|m| m.get("aginx/agentId").or(m.get("agentId")))
                    .and_then(|v| v.as_str())
                    .map(String::from)
            })
            .unwrap_or_else(|| "unknown".to_string());

        let agent_info = match self.agent_manager.get_agent_info(&agent_id).await {
            Some(info) => info,
            None => return AcpResponse::error(request.id, -32002, "Agent info not found"),
        };

        let adapter = self.get_adapter(&agent_info).await;

        // Build clean ACP params — only standard session/load fields
        let mut acp_params = serde_json::json!({
            "sessionId": session_id,
        });
        // Resolve cwd: from client params → conversation data → $HOME
        // Validate the path actually exists (Gemini scanner may produce invalid paths like "/sophiehe")
        let mut resolved_cwd = params.get("cwd")
            .and_then(|v| v.as_str())
            .map(String::from);
        if resolved_cwd.is_none() {
            let conversations = self.collect_conversations(&agent_id).await;
            let workdir = conversations.iter().find(|c| {
                c.get("sessionId").and_then(|v| v.as_str()) == Some(session_id.as_str())
            }).and_then(|c| c.get("workdir").and_then(|v| v.as_str()).map(String::from));
            resolved_cwd = workdir;
        }
        let cwd = match resolved_cwd {
            Some(ref path) if std::path::Path::new(path).is_dir() => path.clone(),
            Some(ref path) => {
                tracing::warn!("[LOAD] cwd '{}' does not exist, falling back to $HOME", path);
                dirs::home_dir().map(|h| h.to_string_lossy().to_string()).unwrap_or("/".to_string())
            }
            None => dirs::home_dir().map(|h| h.to_string_lossy().to_string()).unwrap_or("/".to_string()),
        };
        acp_params["cwd"] = serde_json::Value::String(cwd);
        // mcpServers: Copilot requires this field (even as empty array)
        if let Some(mcpServers) = params.get("mcpServers") {
            acp_params["mcpServers"] = mcpServers.clone();
        } else {
            acp_params["mcpServers"] = serde_json::json!([]);
        }

        tracing::info!("[LOAD] sending session/load to agent {}: {}", agent_id, serde_json::to_string(&acp_params).unwrap_or_default());

        // Pre-register session → agent mapping for prompt routing
        self.acp_session_map.lock().unwrap().insert(session_id.clone(), agent_id.clone());

        // Forward to ACP agent — transparent passthrough with clean params
        match adapter.send_request("session/load", acp_params).await {
            Ok(mut result) => {
                tracing::info!("ACP loadSession: forwarded to agent {}", agent_id);
                if let Some(obj) = result.as_object_mut() {
                    obj.insert("agentId".to_string(), serde_json::Value::String(agent_id.clone()));
                }
                if let Some(sid) = result.get("sessionId").and_then(|v| v.as_str()) {
                    self.acp_session_map.lock().unwrap().insert(sid.to_string(), agent_id.clone());
                } else {
                    self.acp_session_map.lock().unwrap().insert(session_id.clone(), agent_id.clone());
                }
                return AcpResponse::success(request.id, result);
            }
            Err(e) => {
                if agent_info.protocol != "claude-stream" {
                    tracing::error!("[LOAD] session/load FAILED for agent {}: error='{}' sessionId='{}'", agent_id, e, session_id);
                    return AcpResponse::error(request.id, -32603, &format!("Agent error: {}", e));
                }
                tracing::info!("[LOAD] Claude fallback: {}", e);
            }
        }

        // Claude fallback: create session in session_manager with the agent session ID
        let agent_session_id = &session_id;
        tracing::info!("[LOAD] Claude fallback: agent_session_id={}", agent_session_id);

        let workdir = params.get("cwd").and_then(|v| v.as_str()).map(String::from);

        let session_id = match self.session_manager.create_session_with_agent_id(
            agent_session_id,
            &agent_info,
            workdir.as_deref(),
        ).await {
            Ok(id) => id,
            Err(e) => return AcpResponse::error(request.id, -32603, &format!("Failed to create session: {}", e)),
        };

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
        let params: PromptParams = match request.parse_params() {
            Ok(p) => p,
            Err(resp) => return resp,
        };

        // Check if session's agent is accessible (closed trusted network)
        if auth == ConnectionAuth::Pending {
            return AcpResponse::error(request.id, -32001, "Authentication required");
        }

        // Extract text from prompt blocks
        let message: String = params.prompt.iter()
            .filter_map(|block| {
                if let ContentBlock::Text { text, .. } = block {
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

        // Trace: log the full request params for debugging agentId
        tracing::info!("[TRACE] prompt sessionId={} _meta={:?} params.agentId={:?}",
            params.sessionId,
            params._meta,
            request.params.as_ref().and_then(|p| p.get("agentId")).and_then(|v| v.as_str()));

        // Resolve agent_id: from session_manager (Claude) or _meta (ACP agents)
        let agent_id = match self.session_manager.get_session_info(&params.sessionId).await {
            Some(info) => {
                self.session_manager.touch_session(&params.sessionId).await;
                tracing::info!("[TRACE] prompt → session_manager resolved agentId={}", info.agent_id);
                info.agent_id.clone()
            }
            None => {
                // ACP agent: check session map first, then _meta
                if let Some(aid) = self.acp_session_map.lock().unwrap().get(&params.sessionId).cloned() {
                    tracing::info!("[TRACE] prompt → acp_session_map resolved agentId={}", aid);
                    aid
                } else {
                    let from_meta = params._meta.as_ref()
                        .and_then(|m| m.get("aginx/agentId").or(m.get("agentId")))
                        .and_then(|v| v.as_str())
                        .map(String::from);
                    if let Some(ref aid) = from_meta {
                        tracing::info!("[TRACE] prompt → _meta resolved agentId={}", aid);
                    } else {
                        tracing::warn!("[TRACE] prompt → NO agentId found for session {}! _meta={:?}", params.sessionId, params._meta);
                    }
                    from_meta.unwrap_or_else(|| {
                        request.params.as_ref()
                            .and_then(|p| p.get("agentId"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown")
                            .to_string()
                    })
                }
            }
        };

        // Get agent info
        let agent_info = match self.agent_manager.get_agent_info(&agent_id).await {
            Some(agent) => agent,
            None => {
                return AcpResponse::error(request.id, -32603, &format!("Agent not found: {}", agent_id));
            }
        };

        // Get cached adapter based on agent config
        let adapter = self.get_adapter(&agent_info).await;

        // Dispatch prompt to adapter (simplified signature)
        if let Err(e) = adapter.prompt(
            &params.sessionId,
            &message,
            request.id.clone(),
            tx,
        ).await {
            return AcpResponse::error(request.id, -32603, &format!("Prompt error: {}", e));
        }

        // Return immediately - streaming happens in background
        AcpResponse::success(request.id, serde_json::json!({
            "streaming": true,
            "sessionId": params.sessionId
        }))
    }

    /// Handle setMode request — acknowledge mode switch
    async fn handle_set_mode(&self, request: AcpRequest) -> AcpResponse {
        use super::types::SetModeParams;
        let params: SetModeParams = match request.parse_params() {
            Ok(p) => p,
            Err(resp) => return resp,
        };
        tracing::debug!("ACP setMode: sessionId={}, modeId={}", params.sessionId, params.modeId);
        AcpResponse::success(request.id, serde_json::json!({}))
    }

    /// Handle cancel request — forward to ACP agent
    async fn handle_cancel(&self, request: AcpRequest) -> AcpResponse {
        use super::types::CancelParams;

        let params: CancelParams = match request.parse_params() {
            Ok(p) => p,
            Err(resp) => return resp,
        };

        tracing::info!("ACP cancel: sessionId={}", params.sessionId);

        // Try to resolve agent via session_manager or _meta
        let agent_id = match self.session_manager.get_session_info(&params.sessionId).await {
            Some(info) => Some(info.agent_id.clone()),
            None => request.params.as_ref()
                .and_then(|p| p.get("_meta"))
                .and_then(|m| m.get("aginx/agentId").or(m.get("agentId")))
                .and_then(|v| v.as_str())
                .map(String::from),
        };

        if let Some(agent_id) = agent_id {
            if let Some(agent_info) = self.agent_manager.get_agent_info(&agent_id).await {
                let adapter = self.get_adapter(&agent_info).await;
                // Forward cancel to agent (ACP agent) — ignore errors (adapter may not support it)
                let _ = adapter.send_request("session/cancel", serde_json::json!({
                    "sessionId": params.sessionId,
                })).await;
            }
        }

        AcpResponse::success(request.id, serde_json::json!({
            "cancelled": true,
            "sessionId": params.sessionId
        }))
    }

    /// Handle permissionResponse — route client's permission decision back to the agent.
    async fn handle_permission_response(&self, request: AcpRequest) -> AcpResponse {
        let params = request.params_value();
        let request_id = match params.get("requestId").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => {
                return AcpResponse::error(request.id, -32602, "Missing requestId");
            }
        };

        let option_id = params.get("optionId").and_then(|v| v.as_str()).map(String::from);

        tracing::info!("Permission response: requestId={}, optionId={:?}", request_id, option_id);

        // Look up the pending permission
        let pending = match self.permission_router.take(&request_id).await {
            Some(p) => p,
            None => {
                return AcpResponse::error(request.id, -32603, &format!("No pending permission for requestId: {}", request_id));
            }
        };

        // Log the resolution
        tracing::info!("Permission resolved: agent={}, session={}, optionId={:?}",
            pending.agent_id, pending.aginx_session_id, option_id);

        AcpResponse::success(request.id, serde_json::json!({
            "resolved": true,
            "requestId": request_id
        }))
    }

    /// Collect all conversations for an agent (adapter → aginx metadata)
    async fn collect_conversations(&self, agent_id: &str) -> Vec<serde_json::Value> {
        let agent_info = self.agent_manager.get_agent_info(agent_id).await;

        // Path 1: adapter (ACP RPC or config-driven scan)
        if let Some(ref info) = agent_info {
            let adapter = self.get_adapter(info).await;
            match adapter.list_sessions().await {
                Ok(sessions) if !sessions.is_empty() => {
                    tracing::debug!("[collect] agent={} adapter returned {} sessions", agent_id, sessions.len());
                    return sessions;
                }
                Ok(_) => tracing::debug!("[collect] agent={} adapter returned empty", agent_id),
                Err(e) => tracing::debug!("[collect] agent={} adapter error: {}", agent_id, e),
            }
        }

        // Path 2: aginx persisted metadata (Claude sessions)
        self.session_manager.list_persisted_sessions(agent_id).iter().map(|s| {
            serde_json::json!({
                "sessionId": s.session_id,
                "agentId": s.agent_id,
                "workdir": s.workdir,
                "title": s.title,
                "lastMessage": s.last_message,
                "createdAt": s.created_at,
                "updatedAt": s.updated_at,
            })
        }).collect()
    }

    /// Handle listWorkspaces request — list workspaces for an agent
    async fn handle_list_workspaces(&self, request: AcpRequest) -> AcpResponse {
        let params = request.params_value();
        let agent_id = match params.get("agentId").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => {
                return AcpResponse::error(request.id, -32602, "Missing agentId");
            }
        };

        // Collect conversations from all sources (adapter → filesystem → metadata)
        let conversations = self.collect_conversations(&agent_id).await;

        // Group by workdir
        let mut map: std::collections::HashMap<String, (usize, u64)> = std::collections::HashMap::new();
        for conv in &conversations {
            let path = match conv.get("workdir").and_then(|v| v.as_str()) {
                Some(p) if !p.is_empty() => p.to_string(),
                _ => continue,
            };
            let updated = conv.get("updatedAt").and_then(|v| v.as_u64()).unwrap_or(0);
            let entry = map.entry(path).or_insert((0, 0));
            entry.0 += 1;
            entry.1 = entry.1.max(updated);
        }

        let mut workspaces: Vec<serde_json::Value> = map.into_iter().map(|(path, (count, last_active))| {
            let name = std::path::Path::new(&path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(&path)
                .to_string();
            serde_json::json!({
                "path": path,
                "name": name,
                "conversationCount": count,
                "lastActive": last_active,
            })
        }).collect();

        workspaces.sort_by(|a, b| b.get("lastActive").and_then(|v| v.as_u64()).unwrap_or(0)
            .cmp(&a.get("lastActive").and_then(|v| v.as_u64()).unwrap_or(0)));

        AcpResponse::success(request.id, serde_json::json!({
            "workspaces": workspaces
        }))
    }

    /// Handle listSessions request
    async fn handle_list_sessions(&self, request: AcpRequest) -> AcpResponse {
        // Get agent_id from params if provided
        let params = request.params_value();
        let agent_id = params.get("agentId")
            .and_then(|v| v.as_str());
        let cwd = params.get("cwd").and_then(|v| v.as_str());

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

        // Filter by cwd if provided
        let sessions: Vec<_> = if let Some(cwd) = cwd {
            sessions.into_iter().filter(|s| s.workdir.as_deref() == Some(cwd)).collect()
        } else {
            sessions
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
        // Trace: log agent IDs returned to client
        let agent_ids: Vec<&str> = agents.iter()
            .filter_map(|a| a.get("id").and_then(|v| v.as_str()))
            .collect();
        tracing::info!("[TRACE] listAgents → ids: {:?}", agent_ids);

        // Enrich each agent with adapter state
        let adapter_cache = self.adapter_cache.read().await;
        let agents: Vec<serde_json::Value> = agents.into_iter().map(|mut agent| {
            if let Some(agent_id) = agent.get("id").and_then(|v| v.as_str()) {
                if let Some(adapter) = adapter_cache.get(agent_id) {
                    agent["adapterState"] = serde_json::json!(adapter.adapter_state().to_string());
                    if let Some(caps) = adapter.cached_capabilities() {
                        agent["capabilities"] = caps;
                    }
                }
            }
            agent
        }).collect();

        AcpResponse::success(request.id, serde_json::json!({
            "agents": agents,
            "nextCursor": null
        }))
    }

    /// Handle listConversations request - list sessions for an agent
    async fn handle_list_conversations(&self, request: AcpRequest) -> AcpResponse {
        let params = request.params_value();
        let agent_id = match params.get("agentId").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => {
                return AcpResponse::error(request.id, -32602, "Missing agentId");
            }
        };
        let cwd_filter = params.get("cwd").and_then(|v| v.as_str());

        tracing::info!("[listConversations] agentId={}, cwd={:?}", agent_id, cwd_filter);

        let conversations = self.collect_conversations(&agent_id).await;
        tracing::info!("[listConversations] agent={} total={} before filter", agent_id, conversations.len());
        if !conversations.is_empty() {
            tracing::info!("[listConversations] sample workdir={:?}", conversations[0].get("workdir"));
        }

        // Filter by cwd if provided
        let conversations: Vec<_> = if let Some(cwd) = cwd_filter {
            conversations.into_iter().filter(|c| {
                c.get("workdir").and_then(|v| v.as_str()) == Some(cwd)
            }).collect()
        } else {
            conversations
        };

        AcpResponse::success(request.id, serde_json::json!({
            "conversations": conversations
        }))
    }

    /// Handle deleteConversation request
    async fn handle_delete_conversation(&self, request: AcpRequest) -> AcpResponse {
        let params = request.params_value();
        let session_id = match params.get("sessionId").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => {
                return AcpResponse::error(request.id, -32602, "Missing sessionId");
            }
        };

        let agent_id = params.get("agentId").and_then(|v| v.as_str());
        let agent_info = match agent_id {
            Some(aid) => self.agent_manager.get_agent_info(aid).await,
            None => None,
        };

        let deleted = if let Some(ref info) = agent_info {
            let adapter = self.get_adapter(info).await;
            match adapter.delete_session(&session_id).await {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!("Delete session via adapter failed: {}", e);
                    false
                }
            }
        } else {
            false
        };

        tracing::info!("Delete conversation {}: {}", session_id, if deleted { "success" } else { "not found" });

        AcpResponse::success(request.id, serde_json::json!({
            "success": deleted
        }))
    }

    /// Handle getConversationMessages - read messages via adapter
    async fn handle_get_conversation_messages(&self, request: AcpRequest) -> AcpResponse {
        let params = request.params_value();
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

        let agent_id = params.get("agentId").and_then(|v| v.as_str());
        let agent_info = match agent_id {
            Some(aid) => self.agent_manager.get_agent_info(aid).await,
            None => None,
        };

        // Use adapter to get messages (ACP agent RPC or Claude JSONL reading)
        let messages = if let Some(ref info) = agent_info {
            let adapter = self.get_adapter(info).await;
            match adapter.get_messages(&session_id, limit).await {
                Ok(msgs) => msgs,
                Err(e) => {
                    tracing::warn!("Get messages via adapter failed: {}", e);
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };

        tracing::info!("[getConversationMessages] Returning {} messages", messages.len());

        AcpResponse::success(request.id, serde_json::json!({
            "messages": messages
        }))
    }

    /// Handle discoverAgents request - scan directory for aginx.toml files
    async fn handle_discover_agents(&self, request: AcpRequest) -> AcpResponse {
        use crate::agent::scan_directory;

        // Parse params
        let params = request.params_value();
        let scan_path = match params.get("path").and_then(|v| v.as_str()) {
            Some(p) => match Self::safe_resolve_path(p) {
                Ok(resolved) => resolved,
                Err(e) => return AcpResponse::error(request.id, -32600, &e),
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
        let params = request.params_value();
        if params.as_object().map_or(true, |m| m.is_empty()) {
            return AcpResponse::error(request.id, -32602, "Missing params");
        }

        let config_path = match params.get("configPath").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => {
                return AcpResponse::error(request.id, -32602, "configPath is required");
            }
        };

        // Validate path is within home directory
        let path = match Self::safe_resolve_path(config_path) {
            Ok(p) => p,
            Err(e) => return AcpResponse::error(request.id, -32600, &e),
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

    /// Handle discoverRemote request — search aginx-api for remote agents
    async fn handle_discover_remote(&self, request: AcpRequest) -> AcpResponse {
        let api_url = match &self.api_url {
            Some(url) => url.clone(),
            None => return AcpResponse::error(request.id, -32603, "API URL not configured"),
        };

        let params = request.params_value();
        let query = params.get("q").and_then(|v| v.as_str()).unwrap_or("");
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(20);

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        let result = client.get(format!("{}/api/v1/agents/search", api_url))
            .query(&[("q", query), ("limit", &limit.to_string())])
            .send()
            .await;

        match result {
            Ok(resp) => {
                if !resp.status().is_success() {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    return AcpResponse::error(request.id, -32603, &format!("API search failed ({}): {}", status, body));
                }
                match resp.json::<serde_json::Value>().await {
                    Ok(data) => {
                        let agents = data.get("data").and_then(|d| d.get("agents")).cloned();
                        let total = data.get("data").and_then(|d| d.get("total")).cloned();
                        AcpResponse::success(request.id, serde_json::json!({
                            "agents": agents.unwrap_or(serde_json::json!([])),
                            "total": total.unwrap_or(serde_json::json!(0)),
                        }))
                    }
                    Err(e) => AcpResponse::error(request.id, -32603, &format!("Failed to parse API response: {}", e)),
                }
            }
            Err(e) => AcpResponse::error(request.id, -32603, &format!("API request failed: {}", e)),
        }
    }

    /// Handle remote/listAgents — list agents on a remote aginx via AcpClient
    async fn handle_remote_list_agents(&self, request: AcpRequest) -> AcpResponse {
        let token = match &self.aginx_token {
            Some(t) => t.clone(),
            None => return AcpResponse::error(request.id, -32603, "No aginx token configured for remote calls"),
        };

        let params = request.params_value();
        let target = match params.get("target").and_then(|v| v.as_str()) {
            Some(t) => t.to_string(),
            None => return AcpResponse::error(request.id, -32602, "Missing 'target' (aginx address)"),
        };

        let use_tls = params.get("useTls").and_then(|v| v.as_bool()).unwrap_or(true);

        let result = if target.contains(".relay.") || target.contains(".relay.yinnho.cn") {
            // Relay address: extract target ID, use configured relay domain/port
            let target_id = target.split('.').next().unwrap_or(&target);
            let (relay_domain, relay_port) = match &self.relay_domain {
                Some(d) => (d.clone(), self.relay_port),
                None => return AcpResponse::error(request.id, -32603, "Relay not configured"),
            };
            let relay_addr = format!("{}:{}", relay_domain, relay_port);
            super::acp_client::AcpClient::connect_via_relay(&relay_addr, target_id, &token, use_tls).await
        } else {
            super::acp_client::AcpClient::connect(&target, &token).await
        };

        match result {
            Ok(mut client) => match client.list_agents().await {
                Ok(agents) => AcpResponse::success(request.id, serde_json::json!({
                    "agents": agents,
                })),
                Err(e) => AcpResponse::error(request.id, -32603, &format!("Remote listAgents failed: {}", e)),
            },
            Err(e) => AcpResponse::error(request.id, -32603, &format!("Failed to connect to remote aginx: {}", e)),
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
                    "token": device.token,
                    "e2eePublicKey": self.e2ee_public_key(),
                }))
            }
            BindResult::AlreadyBound { device_name } => {
                tracing::warn!("Already bound to device: {}", device_name);
                AcpResponse::error(request.id, -32003, &format!("Already bound to device: {}", device_name))
            }
            BindResult::InvalidCode => {
                tracing::warn!("Invalid or expired pair code: {}", pair_code);
                AcpResponse::error(request.id, -32600, "Invalid or expired pair code")
            }
        }
    }
}
/// Run ACP protocol over stdin/stdout with streaming support
///
/// Security note: stdio mode bypasses authentication (ConnectionAuth::Authenticated)
/// because it's invoked by the local IDE/tool which is inherently trusted.
/// Do NOT expose stdio mode over network without additional auth.
pub async fn run_acp_stdio(agent_manager: Arc<AgentManager>, session_manager: Arc<SessionManager>) {
    let handler = Arc::new(AcpHandler::new(agent_manager, session_manager));
    /// Track sessions with active prompts to prevent concurrent prompt on same session
    let active_prompts: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));

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
                if method == "session/prompt" {
                    // Extract sessionId for concurrency tracking
                    let session_id = request.params.as_ref()
                        .and_then(|p| p.get("sessionId"))
                        .and_then(|v| v.as_str())
                        .map(String::from);

                    // Concurrency guard: reject if session already has an active prompt
                    if let Some(ref sid) = session_id {
                        let mut active = active_prompts.lock().unwrap();
                        if active.contains(sid) {
                            let response = AcpResponse::error(request.id, -32603,
                                &format!("Session {} already has an active prompt", sid));
                            if let Ok(json) = response.to_ndjson() {
                                let mut w = writer.lock().await;
                                let _ = write_ndjson(&mut *w, &json).await;
                            }
                            continue;
                        }
                        active.insert(sid.clone());
                    }

                    // SPAWN: run streaming in a separate task so the main loop
                    // can continue reading the next request (e.g. permissionResponse)
                    let (tx, rx) = mpsc::channel::<String>(32);
                    let handler_clone = handler.clone();
                    let writer_clone = writer.clone();
                    let active_prompts_clone = active_prompts.clone();
                    let session_id_for_cleanup = session_id.clone();

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

                        // Clean up active prompt tracking
                        if let Some(sid) = session_id_for_cleanup {
                            active_prompts_clone.lock().unwrap().remove(&sid);
                        }
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
