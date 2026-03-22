//! aginx-relay - Relay Server for aginx
//!
//! 中继服务器，让内网的 aginx 可以对外提供服务
//!
//! 协议:
//! 1. aginx 注册: {"type":"register"} -> {"type":"registered","id":"xxx","url":"agent://xxx.relay.yinnho.cn"}
//! 2. 客户端连接: {"type":"connect","target":"xxx"} -> {"type":"connected"} 或 {"type":"error","message":"..."}
//! 3. JSON-RPC 消息: 客户端发送纯 JSON-RPC，Relay 内部路由时包装，返回时解包
//!
//! 使用纯 TCP 长连接，不是 WebSocket

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use clap::Parser;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, RwLock};

/// aginx-relay - Relay Server
#[derive(Parser, Debug)]
#[command(name = "aginx-relay")]
#[command(version, about, long_about = None)]
struct Args {
    /// 监听端口 (aginx 连接)
    #[arg(short = 'p', long, default_value = "8600")]
    port: u16,

    /// 监听地址
    #[arg(short = 'H', long, default_value = "0.0.0.0")]
    host: String,

    /// Relay 域名 (用于生成 URL)
    #[arg(long, default_value = "relay.yinnho.cn")]
    domain: String,

    /// 启用调试日志
    #[arg(short = 'd', long)]
    debug: bool,
}

/// 连接消息
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
enum RelayMessage {
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

    /// 连接请求 (client -> relay)
    #[serde(rename = "connect")]
    Connect { target: String },

    /// 连接成功 (relay -> client)
    #[serde(rename = "connected")]
    Connected { target: String },

    /// 断开通知
    #[serde(rename = "disconnected")]
    Disconnected { client_id: String },

    /// 错误
    #[serde(rename = "error")]
    Error { message: String },

    /// 数据消息 (包含 clientId 的 JSON-RPC)
    #[serde(rename = "data")]
    Data { client_id: String, data: serde_json::Value },
}

/// 转发给 aginx 的消息
#[derive(Debug, Clone)]
struct ToAginx {
    client_id: String,
    data: serde_json::Value,
}

/// Relay 服务器状态
struct RelayState {
    /// aginx 连接: aginx_id -> sender
    aginx_senders: HashMap<String, mpsc::Sender<ToAginx>>,
    /// 客户端关联: client_id -> (aginx_id, sender)
    client_senders: HashMap<String, (String, mpsc::Sender<String>)>,
    /// aginx 的客户端列表: aginx_id -> Vec<client_id>
    aginx_clients: HashMap<String, Vec<String>>,
}

impl RelayState {
    fn new() -> Self {
        Self {
            aginx_senders: HashMap::new(),
            client_senders: HashMap::new(),
            aginx_clients: HashMap::new(),
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    init_logging(args.debug);

    let addr: SocketAddr = format!("{}:{}", args.host, args.port).parse()?;
    let listener = TcpListener::bind(addr).await?;

    tracing::info!("========================================");
    tracing::info!("aginx-relay v{}", env!("CARGO_PKG_VERSION"));
    tracing::info!("Listening on {}", addr);
    tracing::info!("Domain: {}", args.domain);
    tracing::info!("========================================");

    let state = Arc::new(RwLock::new(RelayState::new()));
    let domain = Arc::new(args.domain);

    loop {
        let (stream, peer_addr) = listener.accept().await?;
        let state = state.clone();
        let domain = domain.clone();

        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, peer_addr, state, domain).await {
                tracing::error!("Connection error from {}: {}", peer_addr, e);
            }
        });
    }
}

async fn handle_connection(
    stream: tokio::net::TcpStream,
    peer_addr: SocketAddr,
    state: Arc<RwLock<RelayState>>,
    domain: Arc<String>,
) -> anyhow::Result<()> {
    let (reader, writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let writer = Arc::new(tokio::sync::Mutex::new(writer));

    // 读取第一条消息来确定连接类型
    let mut first_line = String::new();
    let bytes = reader.read_line(&mut first_line).await?;
    if bytes == 0 {
        tracing::debug!("No initial message from {}", peer_addr);
        return Ok(());
    }

    let first_line = first_line.trim();
    let json: serde_json::Value = match serde_json::from_str(first_line) {
        Ok(j) => j,
        Err(e) => {
            tracing::error!("Invalid JSON from {}: {}", peer_addr, e);
            send_error(&writer, &format!("Invalid JSON: {}", e)).await;
            return Ok(());
        }
    };

    let msg_type = json.get("type").and_then(|v| v.as_str()).unwrap_or("");

    match msg_type {
        "register" => {
            // 提取用户指定的 ID (可选)
            let requested_id = json.get("id").and_then(|v| v.as_str()).map(|s| s.to_string());
            handle_aginx(reader, writer, state, domain, peer_addr, requested_id).await
        }
        "connect" => {
            let target = json.get("target").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if target.is_empty() {
                send_error(&writer, "Missing target").await;
                return Ok(());
            }
            handle_client(reader, writer, state, target, peer_addr).await
        }
        "ping" => {
            // 简单心跳响应
            send_json(&writer, &serde_json::json!({"type": "pong"})).await
        }
        _ => {
            send_error(&writer, &format!("Unknown message type: {}. First message must be 'register' or 'connect'", msg_type)).await;
            Ok(())
        }
    }
}

async fn handle_aginx(
    mut reader: BufReader<tokio::net::tcp::OwnedReadHalf>,
    writer: Arc<tokio::sync::Mutex<tokio::net::tcp::OwnedWriteHalf>>,
    state: Arc<RwLock<RelayState>>,
    domain: Arc<String>,
    peer_addr: SocketAddr,
    requested_id: Option<String>,
) -> anyhow::Result<()> {
    // 确定使用哪个 ID
    let aginx_id = if let Some(id) = requested_id {
        // 验证 ID 格式 (只允许字母数字)
        if !id.chars().all(|c| c.is_alphanumeric()) {
            send_error(&writer, "ID must be alphanumeric").await;
            return Ok(());
        }
        // 检查 ID 是否已被占用
        let state_read = state.read().await;
        if state_read.aginx_senders.contains_key(&id) {
            drop(state_read);
            send_error(&writer, &format!("ID '{}' is already in use", id)).await;
            return Ok(());
        }
        id
    } else {
        // 自动生成 ID
        generate_id()
    };

    let url = format!("agent://{}.{}", aginx_id, domain);

    tracing::info!("Aginx [{}] registered from {}", aginx_id, peer_addr);

    // 创建接收通道
    let (tx, mut rx) = mpsc::channel::<ToAginx>(100);

    // 注册到状态 (需要再次检查，因为之前释放了锁)
    {
        let mut state = state.write().await;
        if state.aginx_senders.contains_key(&aginx_id) {
            send_error(&writer, &format!("ID '{}' is already in use", aginx_id)).await;
            return Ok(());
        }
        state.aginx_senders.insert(aginx_id.clone(), tx);
        state.aginx_clients.insert(aginx_id.clone(), Vec::new());
    }

    // 发送注册成功
    send_msg(&writer, RelayMessage::Registered {
        id: aginx_id.clone(),
        url: url.clone(),
    }).await?;

    let aginx_id_for_recv = aginx_id.clone();
    let state_for_recv = state.clone();
    let writer_for_send = writer.clone();

    // 发送任务: 从通道接收消息并发送给 aginx
    let send_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let data_msg = RelayMessage::Data {
                client_id: msg.client_id,
                data: msg.data,
            };
            if send_msg(&writer_for_send, data_msg).await.is_err() {
                break;
            }
        }
    });

    // 接收任务: 从 aginx 接收消息
    let recv_task = tokio::spawn(async move {
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }

                    // 解析响应，根据 clientId 转发给对应客户端
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(line) {
                        // 心跳响应
                        if json.get("type").and_then(|v| v.as_str()) == Some("pong") {
                            continue;
                        }

                        if let Some(client_id) = json.get("clientId").and_then(|v| v.as_str()) {
                            let state = state_for_recv.read().await;
                            if let Some((_, tx)) = state.client_senders.get(client_id) {
                                // 剥离 clientId，发送纯 JSON-RPC 响应给客户端
                                let mut pure_response = json.clone();
                                if let Some(obj) = pure_response.as_object_mut() {
                                    obj.remove("clientId");
                                }
                                let _ = tx.send(serde_json::to_string(&pure_response).unwrap()).await;
                            }
                        } else {
                            // 广播给所有客户端 (发送纯 JSON-RPC)
                            let state = state_for_recv.read().await;
                            if let Some(clients) = state.aginx_clients.get(&aginx_id_for_recv) {
                                for client_id in clients {
                                    if let Some((_, tx)) = state.client_senders.get(client_id) {
                                        let _ = tx.send(serde_json::to_string(&json).unwrap()).await;
                                    }
                                }
                            }
                        }
                    }
                }
                Err(_) => break,
            }
        }
    });

    // 等待任一任务结束
    tokio::select! {
        _ = send_task => {}
        _ = recv_task => {}
    }

    // 清理
    cleanup_aginx(&state, &aginx_id).await;
    tracing::info!("Aginx [{}] disconnected", aginx_id);

    Ok(())
}

async fn handle_client(
    mut reader: BufReader<tokio::net::tcp::OwnedReadHalf>,
    writer: Arc<tokio::sync::Mutex<tokio::net::tcp::OwnedWriteHalf>>,
    state: Arc<RwLock<RelayState>>,
    target_aginx: String,
    peer_addr: SocketAddr,
) -> anyhow::Result<()> {
    // 检查目标 aginx 是否在线
    let aginx_tx = {
        let state = state.read().await;
        match state.aginx_senders.get(&target_aginx) {
            Some(tx) => tx.clone(),
            None => {
                send_error(&writer, &format!("Aginx [{}] not found", target_aginx)).await;
                return Ok(());
            }
        }
    };

    // 生成客户端 ID
    let client_id = format!("c_{}", generate_id());

    tracing::info!("Client [{}] connected to Aginx [{}] from {}", client_id, target_aginx, peer_addr);

    // 发送连接成功
    send_msg(&writer, RelayMessage::Connected { target: target_aginx.clone() }).await?;

    // 创建接收通道
    let (tx, mut rx) = mpsc::channel::<String>(100);

    // 注册客户端
    {
        let mut state = state.write().await;
        state.client_senders.insert(client_id.clone(), (target_aginx.clone(), tx));
        if let Some(clients) = state.aginx_clients.get_mut(&target_aginx) {
            clients.push(client_id.clone());
        }
    }

    let client_id_for_recv = client_id.clone();
    let writer_for_send = writer.clone();

    // 发送任务
    let send_task = tokio::spawn(async move {
        while let Some(text) = rx.recv().await {
            let json: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::json!(&text));
            if send_json(&writer_for_send, &json).await.is_err() {
                break;
            }
        }
    });

    // 接收任务
    let recv_task = tokio::spawn(async move {
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    // 转发给 aginx
                    let json = serde_json::from_str(line).unwrap_or_else(|_| serde_json::json!(&line));
                    let _ = aginx_tx.send(ToAginx {
                        client_id: client_id_for_recv.clone(),
                        data: json,
                    }).await;
                }
                Err(_) => break,
            }
        }
    });

    // 等待任一任务结束
    tokio::select! {
        _ = send_task => {}
        _ = recv_task => {}
    }

    // 清理
    cleanup_client(&state, &client_id, &target_aginx).await;
    tracing::info!("Client [{}] disconnected from Aginx [{}]", client_id, target_aginx);

    Ok(())
}

async fn cleanup_aginx(state: &Arc<RwLock<RelayState>>, aginx_id: &str) {
    let mut state = state.write().await;
    state.aginx_senders.remove(aginx_id);

    // 获取并清理关联的客户端
    if let Some(clients) = state.aginx_clients.remove(aginx_id) {
        for client_id in clients {
            state.client_senders.remove(&client_id);
        }
    }
}

async fn cleanup_client(state: &Arc<RwLock<RelayState>>, client_id: &str, aginx_id: &str) {
    let mut state = state.write().await;
    state.client_senders.remove(client_id);
    if let Some(clients) = state.aginx_clients.get_mut(aginx_id) {
        clients.retain(|id| id != client_id);
    }
}

async fn send_msg(
    writer: &Arc<tokio::sync::Mutex<tokio::net::tcp::OwnedWriteHalf>>,
    msg: RelayMessage,
) -> anyhow::Result<()> {
    let json = serde_json::to_value(msg)?;
    send_json(writer, &json).await
}

async fn send_json(
    writer: &Arc<tokio::sync::Mutex<tokio::net::tcp::OwnedWriteHalf>>,
    value: &serde_json::Value,
) -> anyhow::Result<()> {
    let text = serde_json::to_string(value)?;
    let mut w = writer.lock().await;
    w.write_all(format!("{}\n", text).as_bytes()).await?;
    w.flush().await?;
    Ok(())
}

async fn send_error(
    writer: &Arc<tokio::sync::Mutex<tokio::net::tcp::OwnedWriteHalf>>,
    message: &str,
) {
    let _ = send_msg(writer, RelayMessage::Error { message: message.to_string() }).await;
}

fn generate_id() -> String {
    use rand::Rng;
    const CHARSET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::thread_rng();
    (0..8).map(|_| CHARSET[rng.gen_range(0..CHARSET.len())] as char).collect()
}

fn init_logging(debug: bool) {
    tracing_subscriber::fmt()
        .with_max_level(if debug { tracing::Level::DEBUG } else { tracing::Level::INFO })
        .with_target(false)
        .compact()
        .init();
}
