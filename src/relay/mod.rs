//! Relay client for aginx
//!
//! 连接到 relay 服务器，让内网的 aginx 可以对外提供服务
//! 使用纯 TCP 长连接，不是 WebSocket

use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::Mutex;

use crate::config::Config;
use crate::protocol::{JsonRpcRequest, JsonRpcResponse, JsonRpcErrorObject};

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
    #[serde(rename = "register")]
    Register { id: Option<String> },

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

/// Relay 客户端
pub struct RelayClient {
    /// Relay 完整地址 (格式: {id}.relay.yinnho.cn:8600)
    relay_url: String,
    /// Aginx ID (从 relay_url 提取)
    aginx_id: String,
    /// 心跳间隔 (秒)
    heartbeat_interval: u64,
    /// 重连间隔 (秒)
    reconnect_interval: u64,
}

impl RelayClient {
    /// 创建新的 relay 客户端
    pub fn new(config: &Config) -> Self {
        let relay_url = config.relay.url.clone()
            .unwrap_or_else(|| "xxx.relay.yinnho.cn:8600".to_string());

        // 从 URL 提取 aginx_id (格式: {id}.relay.yinnho.cn:8600)
        let aginx_id = relay_url
            .split('.')
            .next()
            .unwrap_or("unknown")
            .to_string();

        Self {
            relay_url,
            aginx_id,
            heartbeat_interval: config.relay.heartbeat_interval,
            reconnect_interval: config.relay.reconnect_interval,
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

        // 发送注册请求 (携带用户指定的 ID)
        let register = RelayMessage::Register { id: Some(self.aginx_id.clone()) };
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
                                if let Err(e) = handle_data_message(&writer, &client_id, data, &config).await {
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
) -> anyhow::Result<()> {
    // 解析 JSON-RPC 请求
    let request: JsonRpcRequest = serde_json::from_value(data.clone())
        .unwrap_or_else(|_| JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: None,
            method: String::new(),
            params: None,
        });

    // 处理请求
    let response = process_request(request, config).await;

    // 发送响应 (带 clientId)
    if let Some(resp) = response {
        let resp_json = serde_json::to_value(&resp)?;
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
    }

    Ok(())
}

/// 处理 JSON-RPC 请求
async fn process_request(request: JsonRpcRequest, config: &Arc<Config>) -> Option<JsonRpcResponse> {
    let id = request.id.clone();

    match request.method.as_str() {
        "hello" => {
            Some(JsonRpcResponse::success(id, serde_json::json!({
                "serverName": config.server.name,
                "serverVersion": config.server.version
            })))
        }
        "getServerInfo" => {
            Some(JsonRpcResponse::success(id, serde_json::json!({
                "version": config.server.version,
                "agentCount": 2,
                "relayUrl": None::<String>
            })))
        }
        "listAgents" => {
            Some(JsonRpcResponse::success(id, serde_json::json!({
                "agents": [
                    {"id": "echo", "name": "Echo Agent", "capabilities": ["echo"]},
                    {"id": "info", "name": "Info Agent", "capabilities": ["info"]}
                ]
            })))
        }
        "sendMessage" => {
            Some(JsonRpcResponse::success(id, serde_json::json!({
                "response": "Message received via relay"
            })))
        }
        "" => None, // 空方法，忽略
        _ => {
            Some(JsonRpcResponse::error(id, JsonRpcErrorObject::method_not_found(&request.method)))
        }
    }
}
