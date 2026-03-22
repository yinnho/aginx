//! Request handler for aginx

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::net::TcpStream;

use crate::config::Config;
use crate::agent::AgentManager;
use crate::protocol::{JsonRpcRequest, JsonRpcResponse, JsonRpcErrorObject, RequestId};

/// Request handler
pub struct Handler {
    config: Arc<Config>,
    agent_manager: AgentManager,
}

impl Handler {
    /// Create a new handler
    pub fn new(config: Arc<Config>, agent_manager: AgentManager) -> Self {
        Self { config, agent_manager }
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
            "getServerInfo" => {
                self.handle_get_server_info(&request, id).await
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

        let agent_id = params.get("agentId")
            .and_then(|v| v.as_str())
            .unwrap_or_default();

        let message = params.get("message")
            .and_then(|v| v.as_str())
            .unwrap_or_default();

        // Send to agent
        match self.agent_manager.send_message(agent_id, message).await {
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
