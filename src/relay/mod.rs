//! Relay client for aginx
//!
//! Connects to relay server, registers, and forwards prompt requests to local agents.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, AsyncRead, AsyncWrite, BufReader};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, mpsc};

use crate::agent::AgentManager;
use crate::config::Config;
use crate::acp::{Handler as AcpHandler, AcpRequest, AcpResponse};
use crate::auth::AuthLevel;

/// Relay message types
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum RelayMessage {
    #[serde(rename = "ping")]
    Ping,
    #[serde(rename = "pong")]
    Pong,
    #[serde(rename = "register")]
    Register { id: String, token: Option<String> },
    #[serde(rename = "registered")]
    Registered { id: String, url: String },
    #[serde(rename = "disconnected")]
    Disconnected { client_id: String },
    #[serde(rename = "error")]
    Error { message: String },
    #[serde(rename = "data")]
    Data { client_id: String, data: serde_json::Value },
    #[serde(rename = "connect")]
    Connect { target: String, token: Option<String> },
    #[serde(rename = "connected")]
    Connected { client_id: String },
}

/// Registration result from API
/// Relay client
pub struct RelayClient {
    relay_url: String,
    tls_domain: String,
    use_tls: bool,
    aginx_id: String,
    heartbeat_interval: u64,
    reconnect_interval: u64,
    handler: Arc<AcpHandler>,
    client_auth: Arc<Mutex<HashMap<String, Option<AuthLevel>>>>,
    access: crate::config::AccessMode,
    relay_secret: Option<String>,
}

impl RelayClient {
    pub fn new(config: &Config, agent_manager: AgentManager) -> Self {
        let relay_url = config.relay.get_connect_url();
        let use_tls = config.relay.use_tls;
        let aginx_id = config.relay.id.clone().unwrap_or_else(|| "unknown".to_string());
        let agent_manager = Arc::new(agent_manager);
        let handler = Arc::new(
            AcpHandler::with_access(config.server.access, config.server.auto_approve, (*agent_manager).clone())
                .with_jwt_secret(config.auth.jwt_secret.clone())
        );

        Self {
            relay_url,
            tls_domain: config.relay.domain.clone(),
            use_tls,
            aginx_id,
            heartbeat_interval: config.relay.heartbeat_interval,
            reconnect_interval: config.relay.reconnect_interval,
            handler,
            client_auth: Arc::new(Mutex::new(HashMap::new())),
            access: config.server.access,
            relay_secret: config.relay.relay_secret.clone(),
        }
    }

    /// Connect and reconnect loop
    pub async fn connect(&mut self) -> anyhow::Result<()> {
        tracing::info!("Connecting to relay: {} (TLS: {})", self.relay_url, self.use_tls);

        loop {
            match self.connect_once().await {
                Ok(_) => tracing::warn!("Relay disconnected, reconnecting..."),
                Err(e) => tracing::error!("Relay error: {}", e),
            }
            tracing::info!("Reconnecting in {}s...", self.reconnect_interval);
            tokio::time::sleep(Duration::from_secs(self.reconnect_interval)).await;
        }
    }

    async fn connect_once(&self) -> anyhow::Result<()> {
        let tcp_stream = TcpStream::connect(&self.relay_url).await?;
        tracing::info!("TCP connected: {}", self.relay_url);

        if self.use_tls {
            self.connect_with_tls(tcp_stream).await
        } else {
            self.connect_with_stream(tcp_stream).await
        }
    }

    async fn connect_with_tls(&self, tcp_stream: TcpStream) -> anyhow::Result<()> {
        let domain = self.tls_domain.clone();
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let config = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
        let server_name = rustls_pki_types::ServerName::try_from(domain)?;
        let tls_stream = connector.connect(server_name, tcp_stream).await?;
        tracing::info!("TLS handshake successful");

        let (reader, writer) = tokio::io::split(tls_stream);
        let mut reader = BufReader::new(reader);
        let writer: Arc<Mutex<Box<dyn AsyncWrite + Unpin + Send>>> =
            Arc::new(Mutex::new(Box::new(writer)));

        self.relay_handshake(&mut reader, &writer).await?;
        self.message_loop(&mut reader, &writer).await
    }

    async fn connect_with_stream(&self, tcp_stream: TcpStream) -> anyhow::Result<()> {
        let (reader, writer) = tcp_stream.into_split();
        let mut reader = BufReader::new(reader);
        let writer: Arc<Mutex<Box<dyn AsyncWrite + Unpin + Send>>> =
            Arc::new(Mutex::new(Box::new(writer)));

        self.relay_handshake(&mut reader, &writer).await?;
        self.message_loop(&mut reader, &writer).await
    }

    async fn relay_handshake<R: AsyncBufReadExt + AsyncRead + Unpin>(
        &self,
        reader: &mut R,
        writer: &Arc<Mutex<Box<dyn AsyncWrite + Unpin + Send>>>,
    ) -> anyhow::Result<()> {
        let register = RelayMessage::Register { id: self.aginx_id.clone(), token: self.relay_secret.clone() };
        let register_json = serde_json::to_string(&register)?;
        {
            let mut w = writer.lock().await;
            w.write_all(format!("{}\n", register_json).as_bytes()).await?;
            w.flush().await?;
        }
        tracing::debug!("Sent registration (credentials redacted)");

        let mut response_line = String::new();
        reader.read_line(&mut response_line).await?;
        let response: RelayMessage = serde_json::from_str(response_line.trim())?;

        match response {
            RelayMessage::Registered { id, url } => {
                tracing::info!("========================================");
                tracing::info!("Registered! ID: {}, URL: {}", id, url);
                tracing::info!("========================================");
            }
            RelayMessage::Error { message } => {
                return Err(anyhow::anyhow!("Registration failed: {}", message));
            }
            _ => {
                return Err(anyhow::anyhow!("Unexpected response: {:?}", response));
            }
        }

        Ok(())
    }

    async fn message_loop<R: AsyncBufReadExt + AsyncRead + Unpin>(
        &self,
        reader: &mut R,
        writer: &Arc<Mutex<Box<dyn AsyncWrite + Unpin + Send>>>,
    ) -> anyhow::Result<()> {
        // Heartbeat
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
                        break;
                    }
                }
            }
        });

        // Message loop
        const MAX_RELAY_LINE: usize = 128 * 1024 * 1024; // 128MB — 借用轮 ticket/materials/files 走单行 JSON
        loop {
            let mut line = String::new();
            match crate::server::handler::read_line_with_limit(reader, &mut line, MAX_RELAY_LINE).await {
                Ok(0) => {
                    tracing::info!("Relay connection closed");
                    break;
                }
                Ok(_) => {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }

                    if let Ok(msg) = serde_json::from_str::<RelayMessage>(line) {
                        match msg {
                            RelayMessage::Pong => {}
                            RelayMessage::Data { client_id, data } => {
                                tracing::debug!("Data from client [{}]", client_id);
                                if let Err(e) = handle_data_message(
                                    writer, &client_id, data, &self.handler, &self.client_auth, &self.access,
                                ).await {
                                    tracing::error!("Error handling message: {}", e);
                                }
                            }
                            RelayMessage::Disconnected { client_id } => {
                                tracing::info!("Client [{}] disconnected", client_id);
                                self.client_auth.lock().await.remove(&client_id);
                            }
                            RelayMessage::Error { message } => {
                                tracing::error!("Relay error: {}", message);
                            }
                            _ => {
                                tracing::warn!("Received unexpected relay message type");
                            }
                        }
                    } else {
                        tracing::warn!("Failed to parse relay message: {}", line);
                    }
                }
                Err(e) => {
                    tracing::error!("Read error: {}", e);
                    break;
                }
            }
        }

        heartbeat_handle.abort();

        Ok(())
    }
}

/// Handle data message from relay
async fn handle_data_message(
    writer: &Arc<Mutex<Box<dyn AsyncWrite + Unpin + Send>>>,
    client_id: &str,
    data: serde_json::Value,
    handler: &Arc<AcpHandler>,
    client_auth: &Arc<Mutex<HashMap<String, Option<AuthLevel>>>>,
    access: &crate::config::AccessMode,
) -> anyhow::Result<()> {
    let request: AcpRequest = match serde_json::from_value(data) {
        Ok(req) => req,
        Err(e) => {
            let response = AcpResponse::error(None, -32700, &format!("Parse error: {}", e));
            send_relay_response(writer, client_id, &response).await?;
            return Ok(());
        }
    };

    let method = request.method.clone();

    // Get auth state
    let auth = {
        let mut states = client_auth.lock().await;
        states.entry(client_id.to_string()).or_insert_with(|| {
            if matches!(access, crate::config::AccessMode::Public) {
                Some(AuthLevel::Bound)
            } else {
                None
            }
        }).clone()
    };

    if method == "prompt" {
        let handler = handler.clone();
        let writer = writer.clone();
        let client_id = client_id.to_string();
        let (tx, rx) = mpsc::channel::<String>(32);

        tokio::spawn(async move {
            let writer_clone = writer.clone();
            let notify_task = tokio::spawn(async move {
                let mut rx = rx;
                while let Some(notification) = rx.recv().await {
                    let mut w = writer_clone.lock().await;
                    let msg = format!("{}\n", notification);
                    if w.write_all(msg.as_bytes()).await.is_err() || w.flush().await.is_err() {
                        break;
                    }
                }
            });

            let response = handler.handle_prompt(request, tx, auth).await;

            if response.result.as_ref().and_then(|r| r.get("streaming")).is_none() {
                if let Err(e) = send_relay_response(&writer, &client_id, &response).await {
                    tracing::error!("Failed to send prompt error: {}", e);
                }
            }

            let _ = notify_task.await;
        });
    } else {
        let (response, new_auth) = handler.handle_request(request, auth).await;
        // Save updated auth state
        if let Some(new_auth_state) = new_auth {
            let mut states = client_auth.lock().await;
            states.insert(client_id.to_string(), Some(new_auth_state));
        }
        send_relay_response(writer, client_id, &response).await?;
    }

    Ok(())
}

/// Send response via relay (wrapped with clientId)
async fn send_relay_response(
    writer: &Arc<Mutex<Box<dyn AsyncWrite + Unpin + Send>>>,
    client_id: &str,
    response: &AcpResponse,
) -> anyhow::Result<()> {
    let resp_json = serde_json::to_value(response)?;
    let mut wrapped = serde_json::json!({
        "clientId": client_id,
    });
    if let Some(obj) = resp_json.as_object() {
        for (k, v) in obj {
            wrapped[k.clone()] = v.clone();
        }
    }
    let resp_text = serde_json::to_string(&wrapped)?;

    let mut w = writer.lock().await;
    w.write_all(format!("{}\n", resp_text).as_bytes()).await?;
    w.flush().await?;
    Ok(())
}
