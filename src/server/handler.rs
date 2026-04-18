//! Request handler for aginx TCP server (ACP Protocol)
//!
//! This handler implements the ACP (Agent Client Protocol) over TCP.
//! ACP uses JSON-RPC 2.0 with ndjson (newline-delimited JSON) format.
//!
//! AUTH: Each connection maintains a ConnectionAuth state.
//! - Pending: only public agents + bindDevice accessible
//! - Authenticated: full access

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader, BufWriter};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use crate::config::Config;
use crate::config::AccessMode;
use crate::agent::{AgentManager, SessionManager};
use crate::acp::{AcpHandler, AcpRequest, AcpResponse, ConnectionAuth};
use crate::acp::notifications::write_ndjson;

/// Request handler
pub struct Handler {
    acp_handler: Arc<AcpHandler>,
    config: Arc<Config>,
}

impl Handler {
    /// Create a new handler
    pub fn new(
        config: Arc<Config>,
        agent_manager: AgentManager,
        session_manager: Arc<SessionManager>,
    ) -> Self {
        let agents_dir = config.agents.get_agents_dir();
        tracing::info!("Agents directory: {}", agents_dir.display());

        let agent_manager = Arc::new(agent_manager);
        let acp_handler = Arc::new(
            AcpHandler::with_access(
                agent_manager.clone(),
                session_manager.clone(),
                agents_dir,
                config.server.access,
            )
            .with_jwt_secret(config.auth.jwt_secret.clone())
            .with_api_url(Some(config.api.url.clone()))
            .with_aginx_token(config.relay.token.clone())
            .with_relay_config(config.relay.domain.clone(), config.relay.port)
        );

        Self {
            acp_handler,
            config,
        }
    }

    /// Handle the connection
    pub async fn handle(self, stream: TcpStream, peer_addr: SocketAddr) -> anyhow::Result<()> {
        tracing::info!("ACP connection from {}", peer_addr);

        // Determine initial auth state based on access mode
        let mut auth = if matches!(self.config.server.access, AccessMode::Public) {
            ConnectionAuth::Authenticated
        } else {
            ConnectionAuth::Pending
        };

        let (reader, writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let writer = Arc::new(tokio::sync::Mutex::new(BufWriter::new(writer)));
        let mut line = String::new();

        loop {
            line.clear();

            let bytes_read = tokio::time::timeout(
                Duration::from_secs(300),
                reader.read_line(&mut line)
            ).await.unwrap_or_else(|_| {
                tracing::info!("Connection idle timeout (300s) for {}", peer_addr);
                Ok(0)
            })?;
            if bytes_read == 0 {
                tracing::debug!("ACP connection closed by {}", peer_addr);
                break;
            }

            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            tracing::info!("ACP received from {}: {}", peer_addr, line);

            let request: AcpRequest = match serde_json::from_str(line) {
                Ok(req) => req,
                Err(e) => {
                    tracing::error!("Failed to parse ACP request: {}", e);
                    let response = AcpResponse::error(None, -32700, &format!("Parse error: {}", e));
                    send_response(&writer, &response).await?;
                    continue;
                }
            };

            let method = request.method.clone();

            if method == "session/prompt" {
                // SPAWN: run streaming in a separate task
                let acp_handler = self.acp_handler.clone();
                let writer = writer.clone();
                let (tx, rx) = mpsc::channel::<String>(32);
                let current_auth = auth;

                tokio::spawn(async move {
                    let writer_clone = writer.clone();
                    let notify_task = tokio::spawn(async move {
                        let mut rx = rx;
                        while let Some(notification) = rx.recv().await {
                            let mut w = writer_clone.lock().await;
                            if let Err(e) = write_ndjson(&mut *w, &notification).await {
                                tracing::error!("Failed to write notification: {}", e);
                                break;
                            }
                        }
                    });

                    let response = acp_handler.handle_prompt(request, tx, current_auth).await;

                    if response.result.as_ref().and_then(|r| r.get("streaming")).is_none() {
                        if let Err(e) = send_response(&writer, &response).await {
                            tracing::error!("Failed to send error response: {}", e);
                        }
                    }

                    let _ = notify_task.await;
                });
            } else if method == "session/load" || method == "loadSession" {
                let acp_handler = self.acp_handler.clone();
                let writer = writer.clone();
                let current_auth = auth;

                tokio::spawn(async move {
                    let (response, _) = acp_handler.handle_request(request, current_auth).await;
                    if let Err(e) = send_response(&writer, &response).await {
                        tracing::error!("Failed to send loadSession response: {}", e);
                    }
                });
            } else {
                let (response, upgrade) = self.acp_handler.handle_request(request, auth).await;

                // Upgrade auth if bindDevice succeeded
                if upgrade && matches!(auth, ConnectionAuth::Pending) {
                    tracing::info!("Connection upgraded to Authenticated (device bound)");
                    auth = ConnectionAuth::Authenticated;
                }

                // Check if initialize upgraded auth via token
                if method == "initialize" && matches!(auth, ConnectionAuth::Pending) {
                    if response.result.as_ref()
                        .and_then(|r| r.get("authenticated"))
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                    {
                        tracing::info!("Connection upgraded to Authenticated (token verified)");
                        auth = ConnectionAuth::Authenticated;
                    }
                }

                send_response(&writer, &response).await?;
            }
        }

        Ok(())
    }
}

/// Send ACP response
async fn send_response(
    writer: &Arc<tokio::sync::Mutex<BufWriter<tokio::net::tcp::OwnedWriteHalf>>>,
    response: &AcpResponse
) -> anyhow::Result<()> {
    let json = response.to_ndjson()?;
    tracing::trace!("ACP response: {}", json);
    let mut w = writer.lock().await;
    write_ndjson(&mut *w, &json).await?;
    Ok(())
}
