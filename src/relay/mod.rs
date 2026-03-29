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
use crate::config::{Config, DEFAULT_RELAY_SERVER};
use crate::acp::{AcpHandler, AcpRequest, AcpResponse};

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
    /// ACP Handler (统一协议处理)
    acp_handler: Arc<AcpHandler>,
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

        // 创建 ACP handler (统一协议处理)
        let agents_dir = config.agents.get_agents_dir();
        let acp_handler = Arc::new(AcpHandler::with_agents_dir(
            agent_manager.clone(),
            session_manager.clone(),
            agents_dir,
        ));

        Self {
            relay_url,
            aginx_id,
            heartbeat_interval: config.relay.heartbeat_interval,
            reconnect_interval: config.relay.reconnect_interval,
            agent_manager,
            session_manager,
            acp_handler,
        }
    }

    /// 连接到 relay 服务器
    pub async fn connect(&mut self) -> anyhow::Result<()> {
        tracing::info!("连接 Relay: {}", self.relay_url);

        loop {
            match self.connect_once().await {
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
    async fn connect_once(&self) -> anyhow::Result<()> {
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
                                    &self.acp_handler,
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

/// 处理数据消息 (统一走 ACP, CONCURRENT MODEL)
/// prompt 请求 spawn 独立 task, 主循环可以继续读 permissionResponse
async fn handle_data_message(
    writer: &Arc<Mutex<tokio::net::tcp::OwnedWriteHalf>>,
    client_id: &str,
    data: serde_json::Value,
    acp_handler: &Arc<AcpHandler>,
) -> anyhow::Result<()> {
    // 解析 ACP 请求
    let request: AcpRequest = match serde_json::from_value(data) {
        Ok(req) => req,
        Err(e) => {
            let response = AcpResponse::error(None, -32700, &format!("Parse error: {}", e));
            send_acp_response(writer, client_id, &response).await?;
            return Ok(());
        }
    };

    let method = request.method.clone();

    if method == "prompt" {
        // SPAWN: run streaming in a separate task so the main message loop
        // can continue reading the next message (e.g. permissionResponse)
        let acp_handler = acp_handler.clone();
        let writer = writer.clone();
        let client_id = client_id.to_string();
        let (tx, rx) = mpsc::channel::<String>(32);

        tokio::spawn(async move {
            // Notification forwarder: inject clientId and write to relay
            let writer_clone = writer.clone();
            let client_id_clone = client_id.clone();
            let notify_task = tokio::spawn(async move {
                let mut rx = rx;
                while let Some(notification) = rx.recv().await {
                    let mut notification_value: serde_json::Value =
                        serde_json::from_str(&notification).unwrap_or(serde_json::json!({}));
                    if let Some(obj) = notification_value.as_object_mut() {
                        obj.insert("clientId".to_string(), serde_json::json!(client_id_clone));
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

            // Run streaming
            let response = acp_handler.handle_prompt_streaming(request, tx).await;

            // Wait for all notifications to be written
            let _ = notify_task.await;

            // Send final response
            let _ = send_acp_response(&writer, &client_id, &response).await;
        });

        // Return immediately - main loop continues reading next message
    } else {
        // Non-streaming: handle synchronously
        let response = acp_handler.handle_request(request).await;
        send_acp_response(writer, client_id, &response).await?;
    }

    Ok(())
}

/// 发送 ACP 响应给客户端 (包装 clientId)
async fn send_acp_response(
    writer: &Arc<Mutex<tokio::net::tcp::OwnedWriteHalf>>,
    client_id: &str,
    response: &AcpResponse,
) -> anyhow::Result<()> {
    let resp_json = serde_json::to_value(response)?;
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
