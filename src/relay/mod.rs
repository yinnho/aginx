//! Relay client for aginx
//!
//! 连接到 relay 服务器，让内网的 aginx 可以对外提供服务
//! 使用纯 TCP 长连接，不是 WebSocket

use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, mpsc};

use crate::agent::{AgentManager, SessionConfig, SessionManager};
use crate::binding;
use crate::config::{Config, DEFAULT_RELAY_SERVER};
use crate::protocol::{JsonRpcErrorObject, JsonRpcRequest, JsonRpcResponse};
use crate::acp::{AcpHandler, AcpRequest, Id as AcpId};

/// Relay 消息类型
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum RelayMessage {
    /// 心跳
    #[serde(rename = "ping")]
    Ping,

    /// 心跳响应
    #[serde(rename = "pong")]
    Pong,

    /// 注册请求 (aginx -> relay)
    /// fingerprint: 硬件指纹，用于重置后恢复原 aginx_id
    #[serde(rename = "register")]
    Register {
        id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        fingerprint: Option<String>,
    },

    /// 注册成功 (relay -> aginx)
    #[serde(rename = "registered")]
    Registered { id: String, url: String },

    /// 断开通知
    #[serde(rename = "disconnected")]
    Disconnected { client_id: String },

    /// 错误
    #[serde(rename = "error")]
    Error { message: String },

    /// 数据消息
    #[serde(rename = "data")]
    Data { client_id: String, data: serde_json::Value },
}

/// 注册结果
#[derive(Debug, Clone)]
pub struct Registration {
    pub id: String,
    pub url: String,
}

/// 向 relay 申请 ID（首次启动时调用）
pub async fn register_id() -> anyhow::Result<Registration> {
    tracing::info!("首次启动，正在向 {} 申请 ID...", DEFAULT_RELAY_SERVER);

    // 生成硬件指纹
    let fingerprint = crate::fingerprint::HardwareFingerprint::generate();
    tracing::info!("硬件指纹: {}", fingerprint.as_str());

    // 连接到 relay
    let stream = TcpStream::connect(DEFAULT_RELAY_SERVER).await?;
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    // 发送注册请求（携带硬件指纹，让 relay 分配/恢复 ID）
    let register = RelayMessage::Register {
        id: None,
        fingerprint: Some(fingerprint.as_str().to_string()),
    };
    let register_json = serde_json::to_string(&register)?;
    writer.write_all(format!("{}\n", register_json).as_bytes()).await?;
    writer.flush().await?;

    // 等待响应
    let mut response_line = String::new();
    reader.read_line(&mut response_line).await?;
    let response: RelayMessage = serde_json::from_str(response_line.trim())?;

    match response {
        RelayMessage::Registered { id, url } => {
            tracing::info!("申请成功！ID: {}, URL: {}", id, url);
            Ok(Registration { id, url })
        }
        RelayMessage::Error { message } => {
            Err(anyhow::anyhow!("申请 ID 失败: {}", message))
        }
        _ => {
            Err(anyhow::anyhow!("意外的响应: {:?}", response))
        }
    }
}

/// Relay 客户端
pub struct RelayClient {
    /// Relay 地址
    relay_url: String,
    /// Aginx ID
    aginx_id: String,
    /// 心跳间隔 (秒)
    heartbeat_interval: u64,
    /// 重连间隔 (秒)
    reconnect_interval: u64,
    /// Agent Manager
    agent_manager: Arc<AgentManager>,
    /// Session Manager
    session_manager: Arc<SessionManager>,
}

impl RelayClient {
    /// 创建新的 relay 客户端
    pub fn new(config: &Config, agent_manager: AgentManager) -> Self {
        let relay_url = config.relay.get_connect_url();
        let aginx_id = config
            .relay
            .id
            .clone()
            .unwrap_or_else(|| "unknown".to_string());

        // 创建会话管理器
        let session_config = SessionConfig {
            max_concurrent: config.server.max_concurrent_sessions,
            timeout_seconds: config.server.session_timeout_seconds,
        };
        let session_manager = Arc::new(SessionManager::new(session_config));
        let agent_manager = Arc::new(agent_manager);

        Self {
            relay_url,
            aginx_id,
            heartbeat_interval: config.relay.heartbeat_interval,
            reconnect_interval: config.relay.reconnect_interval,
            agent_manager,
            session_manager,
        }
    }

    /// 连接到 relay 服务器
    pub async fn connect(&mut self, config: Arc<Config>) -> anyhow::Result<()> {
        tracing::info!("连接 Relay: {}", self.relay_url);

        loop {
            match self.connect_once(config.clone()).await {
                Ok(_) => {
                    tracing::warn!("Relay 连接断开，准备重连...");
                }
                Err(e) => {
                    tracing::error!("Relay 连接错误: {}", e);
                }
            }

            // 等待重连
            tracing::info!("{} 秒后重连...", self.reconnect_interval);
            tokio::time::sleep(Duration::from_secs(self.reconnect_interval)).await;
        }
    }

    /// 单次连接
    async fn connect_once(&self, config: Arc<Config>) -> anyhow::Result<()> {
        // 纯 TCP 连接
        let stream = TcpStream::connect(&self.relay_url).await?;
        tracing::info!("TCP 连接成功: {}", self.relay_url);

        let (reader, writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let writer = Arc::new(Mutex::new(writer));

        // 生成硬件指纹
        let fingerprint = crate::fingerprint::HardwareFingerprint::generate();
        tracing::debug!("硬件指纹: {}", fingerprint.as_str());

        // 发送注册请求 (携带已配置的 ID 和硬件指纹)
        let register = RelayMessage::Register {
            id: Some(self.aginx_id.clone()),
            fingerprint: Some(fingerprint.as_str().to_string()),
        };
        let register_json = serde_json::to_string(&register)?;
        {
            let mut w = writer.lock().await;
            w.write_all(format!("{}\n", register_json).as_bytes()).await?;
            w.flush().await?;
        }
        tracing::debug!("发送注册请求: {}", register_json);

        // 等待注册响应
        let mut response_line = String::new();
        reader.read_line(&mut response_line).await?;
        let response: RelayMessage = serde_json::from_str(response_line.trim())?;

        match response {
            RelayMessage::Registered { id, url } => {
                tracing::info!("========================================");
                tracing::info!("注册成功!");
                tracing::info!("ID: {}", id);
                tracing::info!("URL: {}", url);
                tracing::info!("========================================");
            }
            RelayMessage::Error { message } => {
                return Err(anyhow::anyhow!("注册失败: {}", message));
            }
            _ => {
                return Err(anyhow::anyhow!("意外的响应: {:?}", response));
            }
        }

        // 启动心跳任务
        let heartbeat_interval = self.heartbeat_interval;
        let writer_for_heartbeat = writer.clone();
        let heartbeat_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(heartbeat_interval));
            loop {
                interval.tick().await;
                let ping = serde_json::json!({"type": "ping"});
                if let Ok(ping_str) = serde_json::to_string(&ping) {
                    let mut w = writer_for_heartbeat.lock().await;
                    if w.write_all(format!("{}\n", ping_str).as_bytes()).await.is_err()
                        || w.flush().await.is_err()
                    {
                        tracing::debug!("心跳发送失败，连接可能已断开");
                        break;
                    }
                    tracing::trace!("心跳发送成功");
                }
            }
        });

        // 消息处理循环
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line).await {
                Ok(0) => {
                    tracing::info!("Relay 连接关闭");
                    break;
                }
                Ok(_) => {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }

                    tracing::trace!("收到消息: {}", line);

                    if let Ok(msg) = serde_json::from_str::<RelayMessage>(line) {
                        match msg {
                            RelayMessage::Pong => {
                                tracing::trace!("收到心跳响应");
                            }
                            RelayMessage::Data { client_id, data } => {
                                tracing::debug!("收到客户端 [{}] 数据: {:?}", client_id, data);
                                if let Err(e) = handle_data_message(
                                    &writer,
                                    &client_id,
                                    data,
                                    &config,
                                    &self.agent_manager,
                                    &self.session_manager,
                                )
                                .await
                                {
                                    tracing::error!("处理消息错误: {}", e);
                                }
                            }
                            RelayMessage::Disconnected { client_id } => {
                                tracing::info!("客户端 [{}] 断开连接", client_id);
                            }
                            RelayMessage::Error { message } => {
                                tracing::error!("Relay 错误: {}", message);
                            }
                            _ => {
                                tracing::debug!("未知消息类型: {:?}", msg);
                            }
                        }
                    } else if let Ok(json) = serde_json::from_str::<serde_json::Value>(line) {
                        // 处理普通 JSON 消息
                        if json.get("type").and_then(|t| t.as_str()) == Some("pong") {
                            tracing::trace!("收到心跳响应");
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("读取错误: {}", e);
                    break;
                }
            }
        }

        // 停止心跳任务
        heartbeat_handle.abort();

        Ok(())
    }
}

/// 处理数据消息
async fn handle_data_message(
    writer: &Arc<Mutex<tokio::net::tcp::OwnedWriteHalf>>,
    client_id: &str,
    data: serde_json::Value,
    config: &Arc<Config>,
    agent_manager: &Arc<AgentManager>,
    session_manager: &Arc<SessionManager>,
) -> anyhow::Result<()> {
    // 解析 JSON-RPC 请求
    let request: JsonRpcRequest = serde_json::from_value(data.clone()).unwrap_or_else(|_| {
        JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: None,
            method: String::new(),
            params: None,
        }
    });

    // 检查是否是 ACP 流式方法
    if request.method == "prompt" || request.method == "permissionResponse" {
        // 使用流式处理
        handle_acp_streaming_request(writer, client_id, request, agent_manager, session_manager).await
    } else {
        // 普通请求
        let response = process_request(request, config, agent_manager, session_manager).await;

        // 发送响应 (带 clientId)
        if let Some(resp) = response {
            send_response(writer, client_id, &resp).await?;
        }

        Ok(())
    }
}

/// 发送响应给客户端
async fn send_response(
    writer: &Arc<Mutex<tokio::net::tcp::OwnedWriteHalf>>,
    client_id: &str,
    resp: &JsonRpcResponse,
) -> anyhow::Result<()> {
    let resp_json = serde_json::to_value(resp)?;
    let resp_with_client = serde_json::json!({
        "clientId": client_id,
        "jsonrpc": resp_json.get("jsonrpc").unwrap_or(&serde_json::json!("2.0")),
        "id": resp_json.get("id"),
        "result": resp_json.get("result"),
        "error": resp_json.get("error"),
    });
    let resp_text = serde_json::to_string(&resp_with_client)?;

    let mut w = writer.lock().await;
    w.write_all(format!("{}\n", resp_text).as_bytes()).await?;
    w.flush().await?;

    tracing::debug!("发送响应给客户端 [{}]", client_id);
    Ok(())
}

/// 处理 ACP 流式请求
async fn handle_acp_streaming_request(
    writer: &Arc<Mutex<tokio::net::tcp::OwnedWriteHalf>>,
    client_id: &str,
    request: JsonRpcRequest,
    agent_manager: &Arc<AgentManager>,
    session_manager: &Arc<SessionManager>,
) -> anyhow::Result<()> {
    // 转换 ID 类型
    let acp_id = request.id.as_ref().map(|id| match id {
        crate::protocol::RequestId::String(s) => AcpId::String(s.clone()),
        crate::protocol::RequestId::Number(n) => AcpId::Number(*n),
        crate::protocol::RequestId::Null => AcpId::String("null".to_string()),
    });

    // 转换为 ACP 请求
    let acp_request = AcpRequest {
        jsonrpc: request.jsonrpc.clone(),
        id: acp_id,
        method: request.method.clone(),
        params: request.params,
    };

    // 创建 ACP handler
    let handler = Arc::new(AcpHandler::new(agent_manager.clone(), session_manager.clone()));

    // 创建通知 channel
    let (tx, mut rx) = mpsc::channel::<String>(32);

    // 克隆 writer 用于发送通知
    let writer_clone = writer.clone();
    let client_id_clone = client_id.to_string();

    // 启动通知转发任务
    let notify_handle = tokio::spawn(async move {
        while let Some(notification) = rx.recv().await {
            // 通知已经是 NDJSON 格式，直接发送
            // 合并通知和 clientId
            let mut notification_value: serde_json::Value = serde_json::from_str(&notification).unwrap_or(serde_json::json!({}));
            if let Some(obj) = notification_value.as_object_mut() {
                obj.insert("clientId".to_string(), serde_json::json!(client_id_clone.clone()));
            }
            let resp_text = serde_json::to_string(&notification_value).unwrap_or(notification);

            let mut w = writer_clone.lock().await;
            if let Err(e) = w.write_all(format!("{}\n", resp_text).as_bytes()).await {
                tracing::error!("发送通知失败: {}", e);
                break;
            }
            if let Err(e) = w.flush().await {
                tracing::error!("刷新失败: {}", e);
                break;
            }
        }
    });

    // 处理请求
    let response = if request.method == "prompt" {
        handler.handle_prompt_streaming(acp_request, tx).await
    } else {
        handler.handle_permission_response_streaming(acp_request, tx).await
    };

    // tx 已在 handler 中被消费，channel 会在 handler 完成后关闭
    // 等待通知任务完成
    let _ = notify_handle.await;

    // 转换 ACP 响应为 JSON-RPC 响应
    let id = response.id.map(|id| match id {
        crate::acp::Id::String(s) => crate::protocol::RequestId::String(s),
        crate::acp::Id::Number(n) => crate::protocol::RequestId::Number(n),
    });

    let json_rpc_response = if let Some(error) = response.error {
        JsonRpcResponse::error(id, JsonRpcErrorObject::new(error.code, &error.message, error.data))
    } else {
        JsonRpcResponse::success(id, response.result.unwrap_or(serde_json::json!({})))
    };

    // 发送最终响应
    send_response(writer, client_id, &json_rpc_response).await?;

    Ok(())
}

/// 处理 JSON-RPC 请求
async fn process_request(
    request: JsonRpcRequest,
    config: &Arc<Config>,
    agent_manager: &Arc<AgentManager>,
    session_manager: &Arc<SessionManager>,
) -> Option<JsonRpcResponse> {
    let id = request.id.clone();

    match request.method.as_str() {
        "hello" => Some(JsonRpcResponse::success(
            id,
            serde_json::json!({
                "serverName": config.server.name,
                "serverVersion": config.server.version
            }),
        )),
        "getServerInfo" => {
            let relay_url = config
                .relay
                .id
                .as_ref()
                .map(|id| format!("agent://{}.relay.yinnho.cn", id));
            let agent_count = agent_manager.agent_count().await;
            let session_count = session_manager.session_count().await;
            Some(JsonRpcResponse::success(
                id,
                serde_json::json!({
                    "version": config.server.version,
                    "agentCount": agent_count,
                    "sessionCount": session_count,
                    "relayUrl": relay_url
                }),
            ))
        }
        "listAgents" => {
            let agents = agent_manager.list_agents().await;
            Some(JsonRpcResponse::success(
                id,
                serde_json::json!({
                    "agents": agents
                }),
            ))
        }
        "getAgentHelp" => {
            let agent_id = request
                .params
                .as_ref()
                .and_then(|p| p.get("agentId"))
                .and_then(|v| v.as_str())
                .unwrap_or("");

            if agent_id.is_empty() {
                return Some(JsonRpcResponse::error(
                    id,
                    JsonRpcErrorObject::invalid_params("agentId is required"),
                ));
            }

            match agent_manager.get_agent_help(agent_id).await {
                Ok(help) => Some(JsonRpcResponse::success(
                    id,
                    serde_json::json!({
                        "agentId": agent_id,
                        "help": help
                    }),
                )),
                Err(e) => Some(JsonRpcResponse::error(
                    id,
                    JsonRpcErrorObject::new(404, &e, None),
                )),
            }
        }
        // ACP 风格的 newSession (与 createSession 相同)
        "newSession" | "createSession" => {
            // 支持从 _meta.agentId 或直接从 agentId 获取
            let agent_id = request
                .params
                .as_ref()
                .and_then(|p| p.get("_meta"))
                .and_then(|m| m.get("agentId"))
                .and_then(|v| v.as_str())
                .or_else(|| {
                    request.params.as_ref()
                        .and_then(|p| p.get("agentId"))
                        .and_then(|v| v.as_str())
                })
                .unwrap_or("");

            // 获取可选的 workdir/cwd 参数
            let workdir = request
                .params
                .as_ref()
                .and_then(|p| p.get("cwd").or_else(|| p.get("workdir")))
                .and_then(|v| v.as_str());

            if agent_id.is_empty() {
                return Some(JsonRpcResponse::error(
                    id,
                    JsonRpcErrorObject::invalid_params("agentId is required"),
                ));
            }

            // 获取 agent 信息
            let agent_info = agent_manager.get_agent_info(agent_id).await;

            match agent_info {
                Some(info) => {
                    match session_manager.create_session(&info, workdir).await {
                        Ok(session_id) => Some(JsonRpcResponse::success(
                            id,
                            serde_json::json!({
                                "sessionId": session_id
                            }),
                        )),
                        Err(e) => Some(JsonRpcResponse::error(
                            id,
                            JsonRpcErrorObject::new(503, &e, None), // Service Unavailable
                        )),
                    }
                }
                None => Some(JsonRpcResponse::error(
                    id,
                    JsonRpcErrorObject::new(404, &format!("Agent not found: {}", agent_id), None),
                )),
            }
        }
        "sendMessage" => {
            // 检查是否使用会话模式
            let session_id = request
                .params
                .as_ref()
                .and_then(|p| p.get("sessionId"))
                .and_then(|v| v.as_str());

            if let Some(sid) = session_id {
                // 会话模式
                let message = request
                    .params
                    .as_ref()
                    .and_then(|p| p.get("message"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                match session_manager.send_message(sid, message).await {
                    Ok(response) => Some(JsonRpcResponse::success(
                        id,
                        serde_json::json!({
                            "sessionId": sid,
                            "response": response
                        }),
                    )),
                    Err(e) => Some(JsonRpcResponse::error(
                        id,
                        JsonRpcErrorObject::new(404, &e, None),
                    )),
                }
            } else {
                // 无状态模式（兼容旧接口）
                let agent_id = request
                    .params
                    .as_ref()
                    .and_then(|p| p.get("agentId"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("claude");

                let message = request
                    .params
                    .as_ref()
                    .and_then(|p| p.get("message"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                // 调用对应的 agent
                match agent_manager.send_message(agent_id, message, None).await {
                    Ok(response) => Some(JsonRpcResponse::success(
                        id,
                        serde_json::json!({
                            "response": response
                        }),
                    )),
                    Err(e) => Some(JsonRpcResponse::error(
                        id,
                        JsonRpcErrorObject::new(404, &e, None),
                    )),
                }
            }
        }
        "closeSession" => {
            let session_id = request
                .params
                .as_ref()
                .and_then(|p| p.get("sessionId"))
                .and_then(|v| v.as_str())
                .unwrap_or("");

            if session_id.is_empty() {
                return Some(JsonRpcResponse::error(
                    id,
                    JsonRpcErrorObject::invalid_params("sessionId is required"),
                ));
            }

            match session_manager.close_session(session_id).await {
                Ok(_) => Some(JsonRpcResponse::success(
                    id,
                    serde_json::json!({
                        "success": true,
                        "sessionId": session_id
                    }),
                )),
                Err(e) => Some(JsonRpcResponse::error(
                    id,
                    JsonRpcErrorObject::new(404, &e, None),
                )),
            }
        }
        "bindDevice" => {
            let pair_code = request
                .params
                .as_ref()
                .and_then(|p| p.get("pairCode"))
                .and_then(|v| v.as_str())
                .unwrap_or("");

            let device_name = request
                .params
                .as_ref()
                .and_then(|p| p.get("deviceName"))
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown Device");

            if pair_code.is_empty() {
                return Some(JsonRpcResponse::error(
                    id,
                    JsonRpcErrorObject::invalid_params("pairCode is required"),
                ));
            }

            // 使用单例 BindingManager
            let manager = binding::get_binding_manager();
            let mut binding_manager = manager.lock().unwrap();
            match binding_manager.bind_device(pair_code, device_name) {
                binding::BindResult::Success(device) => Some(JsonRpcResponse::success(
                    id,
                    serde_json::json!({
                        "success": true,
                        "deviceId": device.id,
                        "token": device.token
                    }),
                )),
                binding::BindResult::AlreadyBound { device_name } => Some(JsonRpcResponse::error(
                    id,
                    JsonRpcErrorObject::new(409, &format!("Already bound to device: {}", device_name), None),
                )),
                binding::BindResult::InvalidCode => Some(JsonRpcResponse::error(
                    id,
                    JsonRpcErrorObject::new(400, "Invalid or expired pair code", None),
                )),
            }
        }
        "" => None, // 空方法，忽略
        _ => Some(JsonRpcResponse::error(
            id,
            JsonRpcErrorObject::method_not_found(&request.method),
        )),
    }
}
