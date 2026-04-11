//! Relay client for aginx
//!
//! 连接到 relay 服务器，让内网的 aginx 可以对外提供服务
//! 支持 TLS (通过 nginx stream 终止) 或纯 TCP

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, AsyncRead, AsyncWrite, BufReader};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, mpsc};

use crate::agent::{AgentManager, SessionConfig, SessionManager};
use crate::config::Config;
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
    /// ID 从 API 预先获取，直接告诉 relay
    #[serde(rename = "register")]
    Register {
        id: String,
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

/// 注册结果（从 API 获取）
#[derive(Debug, Clone)]
pub struct Registration {
    pub id: String,
    pub token: String,
}

/// 通过 HTTP 向 API 注册，获取 ID 和 JWT token
pub async fn register_id(api_url: &str) -> anyhow::Result<Registration> {
    tracing::info!("正在向 {} 申请 ID...", api_url);

    // 生成硬件指纹
    let fingerprint = crate::fingerprint::HardwareFingerprint::generate();
    tracing::info!("硬件指纹: {}", fingerprint.as_str());

    // HTTP POST 到 API
    let client = reqwest::Client::builder()
        .build()?;
    let resp = client
        .post(format!("{}/api/v1/instances/register", api_url))
        .json(&serde_json::json!({
            "fingerprint_hash": fingerprint.as_str(),
        }))
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!("API 注册失败 ({}): {}", status, body));
    }

    let result: serde_json::Value = resp.json().await?;
    let data = result.get("data")
        .ok_or_else(|| anyhow::anyhow!("API 响应缺少 data 字段"))?;

    let id = data.get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("API 响应缺少 id"))?
        .to_string();

    let token = data.get("token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("API 响应缺少 token"))?
        .to_string();

    tracing::info!("申请成功！ID: {}", id);
    Ok(Registration { id, token })
}

/// Relay 客户端
pub struct RelayClient {
    /// Relay 地址 (host:port)
    relay_url: String,
    /// TLS 域名 (用于 SNI，可能与 relay_url 的 host 不同)
    tls_domain: String,
    /// 是否使用 TLS
    use_tls: bool,
    /// Aginx ID
    aginx_id: String,
    /// 心跳间隔 (秒)
    heartbeat_interval: u64,
    /// 重连间隔 (秒)
    reconnect_interval: u64,
    /// ACP Handler (统一协议处理)
    acp_handler: Arc<AcpHandler>,
    /// Per-client 认证状态
    client_auth: Arc<Mutex<HashMap<String, crate::acp::ConnectionAuth>>>,
    /// 全局访问模式
    access: crate::config::AccessMode,
}

impl RelayClient {
    /// 创建新的 relay 客户端
    pub fn new(config: &Config, agent_manager: AgentManager) -> Self {
        let relay_url = config.relay.get_connect_url();
        let use_tls = config.relay.use_tls;
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
        let acp_handler = Arc::new(AcpHandler::with_access(
            agent_manager.clone(),
            session_manager.clone(),
            agents_dir,
            config.server.access.clone(),
        ));

        Self {
            relay_url,
            tls_domain: config.relay.domain.clone(),
            use_tls,
            aginx_id,
            heartbeat_interval: config.relay.heartbeat_interval,
            reconnect_interval: config.relay.reconnect_interval,
            acp_handler,
            client_auth: Arc::new(Mutex::new(HashMap::new())),
            access: config.server.access.clone(),
        }
    }

    /// 连接到 relay 服务器
    pub async fn connect(&mut self) -> anyhow::Result<()> {
        tracing::info!("连接 Relay: {} (TLS: {})", self.relay_url, self.use_tls);

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
        // TCP 连接
        let tcp_stream = TcpStream::connect(&self.relay_url).await?;
        tracing::info!("TCP 连接成功: {}", self.relay_url);

        // 根据配置决定是否使用 TLS
        if self.use_tls {
            self.connect_with_tls(tcp_stream).await
        } else {
            self.connect_with_stream(tcp_stream).await
        }
    }

    /// TLS 连接处理
    async fn connect_with_tls(&self, tcp_stream: TcpStream) -> anyhow::Result<()> {
        let domain = self.extract_domain()?;
        let tls_connector = native_tls::TlsConnector::new()?;
        let tls_stream = tokio_native_tls::TlsConnector::from(tls_connector)
            .connect(&domain, tcp_stream)
            .await?;

        tracing::info!("TLS 握手成功");

        let (reader, writer) = tokio::io::split(tls_stream);
        let mut reader = BufReader::new(reader);
        let writer: Arc<Mutex<Box<dyn AsyncWrite + Unpin + Send>>> =
            Arc::new(Mutex::new(Box::new(writer)));

        self.relay_handshake(&mut reader, &writer).await?;
        self.message_loop(&mut reader, &writer).await
    }

    /// 纯 TCP 连接处理
    async fn connect_with_stream(&self, tcp_stream: TcpStream) -> anyhow::Result<()> {
        let (reader, writer) = tcp_stream.into_split();
        let mut reader = BufReader::new(reader);
        let writer: Arc<Mutex<Box<dyn AsyncWrite + Unpin + Send>>> =
            Arc::new(Mutex::new(Box::new(writer)));

        self.relay_handshake(&mut reader, &writer).await?;
        self.message_loop(&mut reader, &writer).await
    }

    /// TLS 域名 (用于 SNI)
    fn extract_domain(&self) -> anyhow::Result<String> {
        Ok(self.tls_domain.clone())
    }

    /// Relay 握手 (注册)
    async fn relay_handshake<R: AsyncBufReadExt + AsyncRead + Unpin>(
        &self,
        reader: &mut R,
        writer: &Arc<Mutex<Box<dyn AsyncWrite + Unpin + Send>>>,
    ) -> anyhow::Result<()> {
        // 发送注册请求 (ID 已从 API 获取)
        let register = RelayMessage::Register {
            id: self.aginx_id.clone(),
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

        Ok(())
    }

    /// 消息处理循环 (启动心跳 + 处理消息)
    async fn message_loop<R: AsyncBufReadExt + AsyncRead + Unpin>(
        &self,
        reader: &mut R,
        writer: &Arc<Mutex<Box<dyn AsyncWrite + Unpin + Send>>>,
    ) -> anyhow::Result<()> {
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
                                    &self.client_auth,
                                    &self.access,
                                )
                                .await
                                {
                                    tracing::error!("处理消息错误: {}", e);
                                }
                            }
                            RelayMessage::Disconnected { client_id } => {
                                tracing::info!("客户端 [{}] 断开连接", client_id);
                                self.client_auth.lock().await.remove(&client_id);
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
    writer: &Arc<Mutex<Box<dyn AsyncWrite + Unpin + Send>>>,
    client_id: &str,
    data: serde_json::Value,
    acp_handler: &Arc<AcpHandler>,
    client_auth: &Arc<Mutex<HashMap<String, crate::acp::ConnectionAuth>>>,
    global_access: &crate::config::AccessMode,
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

    // Get or init auth state for this client
    let auth = {
        let mut states = client_auth.lock().await;
        *states.entry(client_id.to_string()).or_insert_with(|| {
            if matches!(global_access, crate::config::AccessMode::Public) {
                crate::acp::ConnectionAuth::Authenticated
            } else {
                crate::acp::ConnectionAuth::Pending
            }
        })
    };

    if method == "session/prompt" {
        let acp_handler = acp_handler.clone();
        let writer = writer.clone();
        let client_id = client_id.to_string();
        let (tx, rx) = mpsc::channel::<String>(32);

        tokio::spawn(async move {
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

            let _response = acp_handler.handle_prompt(request, tx, auth).await;

            let _ = notify_task.await;
        });
    } else {
        let (response, upgrade) = acp_handler.handle_request(request, auth).await;

        // Upgrade auth if needed
        if upgrade || (method == "initialize" && response.result.as_ref()
            .and_then(|r| r.get("authenticated"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false))
        {
            let mut states = client_auth.lock().await;
            states.insert(client_id.to_string(), crate::acp::ConnectionAuth::Authenticated);
        }

        send_acp_response(writer, client_id, &response).await?;
    }

    Ok(())
}

/// 发送 ACP 响应给客户端 (包装 clientId)
async fn send_acp_response(
    writer: &Arc<Mutex<Box<dyn AsyncWrite + Unpin + Send>>>,
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
