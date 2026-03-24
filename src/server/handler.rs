//! Request handler for aginx

use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Mutex;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::net::TcpStream;

use crate::config::Config;
use crate::agent::{AgentManager, SessionManager};
use crate::binding::BindingManager;
use crate::protocol::{JsonRpcRequest, JsonRpcResponse, JsonRpcErrorObject, RequestId};

/// Request handler
pub struct Handler {
    config: Arc<Config>,
    agent_manager: AgentManager,
    binding_manager: Arc<Mutex<BindingManager>>,
    session_manager: Arc<SessionManager>,
}

impl Handler {
    /// Create a new handler
    pub fn new(
        config: Arc<Config>,
        agent_manager: AgentManager,
        binding_manager: Arc<Mutex<BindingManager>>,
        session_manager: Arc<SessionManager>,
    ) -> Self {
        Self { config, agent_manager, binding_manager, session_manager }
    }

    /// Handle the connection
    pub async fn handle(self, stream: TcpStream, peer_addr: SocketAddr) -> anyhow::Result<()> {
        tracing::info!("Handling connection from {}", peer_addr);

        let (reader, writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let mut writer = BufWriter::new(writer);
        let mut line = String::new();

        loop {
            line.clear();

            // Read a line (JSON-RPC request)
            let bytes_read = reader.read_line(&mut line).await?;
            if bytes_read == 0 {
                tracing::debug!("Connection closed by {}", peer_addr);
                break;
            }

            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            tracing::debug!("Received: {}", line);

            // Parse request
            let request = match JsonRpcRequest::from_json(line) {
                Ok(req) => req,
                Err(e) => {
                    let response = JsonRpcResponse::error(
                        None::<RequestId>,
                        JsonRpcErrorObject::parse_error(e.to_string())
                    );
                    self.send_response(&mut writer, &response).await?;
                    continue;
                }
            };

            // Process request
            let response = self.process_request(request).await;

            // Send response
            if let Some(resp) = response {
                self.send_response(&mut writer, &resp).await?;
            }
        }

        Ok(())
    }

    /// Process a request and return the response
    async fn process_request(&self, request: JsonRpcRequest) -> Option<JsonRpcResponse> {
        let id = request.id.clone();

        match request.method.as_str() {
            "sendMessage" => {
                self.handle_send_message(&request, id).await
            }
            "listAgents" => {
                self.handle_list_agents(&request, id).await
            }
            "getAgentCard" => {
                self.handle_get_agent_card(&request, id).await
            }
            "getAgentConfig" => {
                self.handle_get_agent_config(&request, id).await
            }
            "getServerInfo" => {
                self.handle_get_server_info(&request, id).await
            }
            "bindDevice" => {
                self.handle_bind_device(&request, id).await
            }
            "createSession" => {
                self.handle_create_session(&request, id).await
            }
            "closeSession" => {
                self.handle_close_session(&request, id).await
            }
            "respondPermission" => {
                self.handle_respond_permission(&request, id).await
            }
            _ => {
                Some(JsonRpcResponse::error(id, JsonRpcErrorObject::method_not_found(&request.method)))
            }
        }
    }

    /// Handle sendMessage
    async fn handle_send_message(&self, request: &JsonRpcRequest, id: Option<RequestId>) -> Option<JsonRpcResponse> {
        let params = match &request.params {
            Some(p) => p,
            None => {
                return Some(JsonRpcResponse::error(id, JsonRpcErrorObject::invalid_params("Missing params")));
            }
        };

        // 检查是否使用会话模式
        let session_id = params.get("sessionId")
            .and_then(|v| v.as_str());

        if let Some(sid) = session_id {
            // 会话模式
            let message = params.get("message")
                .and_then(|v| v.as_str())
                .unwrap_or_default();

            match self.session_manager.send_message(sid, message).await {
                Ok(response) => Some(JsonRpcResponse::success(id, serde_json::json!({
                    "sessionId": sid,
                    "response": response
                }))),
                Err(e) => Some(JsonRpcResponse::error(id, JsonRpcErrorObject::new(404, &e, None))),
            }
        } else {
            // 无状态模式
            let agent_id = params.get("agentId")
                .and_then(|v| v.as_str())
                .unwrap_or_default();

            let message = params.get("message")
                .and_then(|v| v.as_str())
                .unwrap_or_default();

            let workdir = params.get("workdir")
                .and_then(|v| v.as_str());

            // Send to agent
            match self.agent_manager.send_message(agent_id, message, workdir).await {
                Ok(response) => {
                    Some(JsonRpcResponse::success(id, serde_json::json!({
                        "response": response
                    })))
                }
                Err(e) => {
                    Some(JsonRpcResponse::error(id, JsonRpcErrorObject::internal_error(&e)))
                }
            }
        }
    }

    /// Handle listAgents
    async fn handle_list_agents(&self, _request: &JsonRpcRequest, id: Option<RequestId>) -> Option<JsonRpcResponse> {
        let agents = self.agent_manager.list_agents().await;
        Some(JsonRpcResponse::success(id, serde_json::json!({
            "agents": agents
        })))
    }

    /// Handle getAgentCard
    async fn handle_get_agent_card(&self, request: &JsonRpcRequest, id: Option<RequestId>) -> Option<JsonRpcResponse> {
        let params = match &request.params {
            Some(p) => p,
            None => {
                return Some(JsonRpcResponse::error(id, JsonRpcErrorObject::invalid_params("Missing params")));
            }
        };

        let agent_id = params.get("agentId")
            .and_then(|v| v.as_str())
            .unwrap_or_default();

        match self.agent_manager.get_agent_card(agent_id).await {
            Some((name, capabilities)) => {
                Some(JsonRpcResponse::success(id, serde_json::json!({
                    "id": agent_id,
                    "name": name,
                    "capabilities": capabilities
                })))
            }
            None => {
                Some(JsonRpcResponse::error(id, JsonRpcErrorObject::new(
                    404,
                    &format!("Agent not found: {}", agent_id),
                    None,
                )))
            }
        }
    }

    /// Handle getAgentConfig - 返回 agent 的完整配置信息
    async fn handle_get_agent_config(&self, request: &JsonRpcRequest, id: Option<RequestId>) -> Option<JsonRpcResponse> {
        let params = match &request.params {
            Some(p) => p,
            None => {
                return Some(JsonRpcResponse::error(id, JsonRpcErrorObject::invalid_params("Missing params")));
            }
        };

        let agent_id = params.get("agentId")
            .and_then(|v| v.as_str())
            .unwrap_or_default();

        match self.agent_manager.get_agent_info(agent_id).await {
            Some(info) => {
                Some(JsonRpcResponse::success(id, serde_json::json!({
                    "id": info.id,
                    "name": info.name,
                    "agentType": format!("{:?}", info.agent_type).to_lowercase(),
                    "capabilities": info.capabilities,
                    "command": info.command,
                    "args": info.args,
                    "helpCommand": info.help_command,
                    "workingDir": info.working_dir,
                    "requireWorkdir": info.require_workdir,
                    "env": info.env
                })))
            }
            None => {
                Some(JsonRpcResponse::error(id, JsonRpcErrorObject::new(
                    404,
                    &format!("Agent not found: {}", agent_id),
                    None,
                )))
            }
        }
    }

    /// Handle getServerInfo
    async fn handle_get_server_info(&self, _request: &JsonRpcRequest, id: Option<RequestId>) -> Option<JsonRpcResponse> {
        // 从 relay.url 提取 agent 地址
        let relay_url = self.config.relay.url.as_ref().and_then(|url| {
            // 格式: abc123.relay.yinnho.cn:8600
            let parts: Vec<&str> = url.split('.').collect();
            parts.first().map(|id| format!("agent://{}.relay.yinnho.cn", id))
        });

        Some(JsonRpcResponse::success(id, serde_json::json!({
            "version": self.config.server.version,
            "agentCount": self.agent_manager.agent_count().await,
            "relayUrl": relay_url
        })))
    }

    /// Handle bindDevice
    async fn handle_bind_device(&self, request: &JsonRpcRequest, id: Option<RequestId>) -> Option<JsonRpcResponse> {
        let params = match &request.params {
            Some(p) => p,
            None => {
                return Some(JsonRpcResponse::error(id, JsonRpcErrorObject::invalid_params("Missing params")));
            }
        };

        let pair_code = params.get("pairCode")
            .and_then(|v| v.as_str())
            .unwrap_or_default();

        let device_name = params.get("deviceName")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown Device");

        // 验证配对码
        let mut manager = self.binding_manager.lock().await;
        match manager.verify_pair_code(pair_code, device_name) {
            Some(device) => {
                tracing::info!("设备已绑定: {} ({})", device.name, device.id);
                Some(JsonRpcResponse::success(id, serde_json::json!({
                    "success": true,
                    "deviceId": device.id,
                    "token": device.token
                })))
            }
            None => {
                Some(JsonRpcResponse::error(id, JsonRpcErrorObject::new(
                    400,
                    "Invalid or expired pair code",
                    None,
                )))
            }
        }
    }

    /// Handle createSession
    async fn handle_create_session(&self, request: &JsonRpcRequest, id: Option<RequestId>) -> Option<JsonRpcResponse> {
        let params = match &request.params {
            Some(p) => p,
            None => {
                return Some(JsonRpcResponse::error(id, JsonRpcErrorObject::invalid_params("Missing params")));
            }
        };

        let agent_id = params.get("agentId")
            .and_then(|v| v.as_str())
            .unwrap_or_default();

        let workdir = params.get("workdir")
            .and_then(|v| v.as_str());

        if agent_id.is_empty() {
            return Some(JsonRpcResponse::error(id, JsonRpcErrorObject::invalid_params("agentId is required")));
        }

        // 获取 agent 信息
        let agent_info = self.agent_manager.get_agent_info(agent_id).await;

        match agent_info {
            Some(info) => {
                match self.session_manager.create_session(&info, workdir).await {
                    Ok(session_id) => Some(JsonRpcResponse::success(id, serde_json::json!({
                        "sessionId": session_id,
                        "agentId": agent_id
                    }))),
                    Err(e) => Some(JsonRpcResponse::error(id, JsonRpcErrorObject::new(503, &e, None))),
                }
            }
            None => Some(JsonRpcResponse::error(id, JsonRpcErrorObject::new(404, &format!("Agent not found: {}", agent_id), None))),
        }
    }

    /// Handle closeSession
    async fn handle_close_session(&self, request: &JsonRpcRequest, id: Option<RequestId>) -> Option<JsonRpcResponse> {
        let params = match &request.params {
            Some(p) => p,
            None => {
                return Some(JsonRpcResponse::error(id, JsonRpcErrorObject::invalid_params("Missing params")));
            }
        };

        let session_id = params.get("sessionId")
            .and_then(|v| v.as_str())
            .unwrap_or_default();

        if session_id.is_empty() {
            return Some(JsonRpcResponse::error(id, JsonRpcErrorObject::invalid_params("sessionId is required")));
        }

        match self.session_manager.close_session(session_id).await {
            Ok(_) => Some(JsonRpcResponse::success(id, serde_json::json!({
                "success": true,
                "sessionId": session_id
            }))),
            Err(e) => Some(JsonRpcResponse::error(id, JsonRpcErrorObject::new(404, &e, None))),
        }
    }

    /// Handle respondPermission - 响应权限请求
    async fn handle_respond_permission(&self, request: &JsonRpcRequest, id: Option<RequestId>) -> Option<JsonRpcResponse> {
        let params = match &request.params {
            Some(p) => p,
            None => {
                return Some(JsonRpcResponse::error(id, JsonRpcErrorObject::invalid_params("Missing params")));
            }
        };

        let session_id = params.get("sessionId")
            .and_then(|v| v.as_str())
            .unwrap_or_default();

        let choice = params.get("choice")
            .and_then(|v| v.as_u64())
            .unwrap_or(1) as usize;

        if session_id.is_empty() {
            return Some(JsonRpcResponse::error(id, JsonRpcErrorObject::invalid_params("sessionId is required")));
        }

        // 发送选择到会话并获取最终响应
        match self.session_manager.respond_permission(session_id, choice).await {
            Ok(response) => Some(JsonRpcResponse::success(id, serde_json::json!({
                "sessionId": session_id,
                "response": response
            }))),
            Err(e) => Some(JsonRpcResponse::error(id, JsonRpcErrorObject::new(500, &e, None))),
        }
    }

    /// Send response
    async fn send_response(&self, writer: &mut BufWriter<tokio::net::tcp::OwnedWriteHalf>, response: &JsonRpcResponse) -> anyhow::Result<()> {
        let json = response.to_json();
        tracing::debug!("Sending: {}", json);
        writer.write_all(json.as_bytes()).await?;
        writer.write_all(b"\n").await?;
        writer.flush().await?;
        Ok(())
    }
}
