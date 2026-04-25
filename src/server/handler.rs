//! Request handler for aginx TCP server
//!
//! Simple JSON-RPC 2.0 over ndjson.
//! Methods: prompt (streaming), listAgents, ping, initialize, bindDevice

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use crate::config::{Config, AccessMode};

/// Read a line with size limit. Returns Ok(bytes_read).
/// If line exceeds max_bytes, returns Ok(0) to signal disconnection.
pub async fn read_line_with_limit<R: AsyncBufReadExt + Unpin>(
    reader: &mut R,
    line: &mut String,
    max_bytes: usize,
) -> std::io::Result<usize> {
    let bytes_read = reader.read_line(line).await?;
    if bytes_read > 0 && line.len() > max_bytes {
        tracing::warn!("Oversized line ({} bytes), dropping connection", line.len());
        return Ok(0);
    }
    Ok(bytes_read)
}use crate::agent::AgentManager;
use crate::acp::{Handler as AcpHandler, AcpRequest, AcpResponse};
use crate::auth::AuthLevel;

/// Request handler
pub struct Handler {
    handler: Arc<AcpHandler>,
    config: Arc<Config>,
}

impl Handler {
    pub fn new(config: Arc<Config>, agent_manager: AgentManager) -> Self {
        let handler = Arc::new(
            AcpHandler::with_access(config.server.access, agent_manager)
                .with_jwt_secret(config.auth.jwt_secret.clone())
        );
        Self { handler, config }
    }

    /// Handle the connection
    pub async fn handle(self, stream: TcpStream, peer_addr: SocketAddr) -> anyhow::Result<()> {
        tracing::info!("Connection from {}", peer_addr);

        let mut auth: Option<AuthLevel> = if matches!(self.config.server.access, AccessMode::Public) {
            Some(AuthLevel::Bound) // Public mode acts as fully authenticated
        } else {
            None
        };

        let (reader, writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let writer = Arc::new(tokio::sync::Mutex::new(BufWriter::new(writer)));
        let mut line = String::new();
        const MAX_LINE_LENGTH: usize = 1024 * 1024; // 1MB

        loop {
            line.clear();

            let bytes_read = tokio::time::timeout(
                Duration::from_secs(300),
                reader.read_line(&mut line),
            ).await.unwrap_or_else(|_| {
                tracing::info!("Idle timeout for {}", peer_addr);
                Ok(0)
            })?;

            if bytes_read == 0 {
                break;
            }

            if line.len() > MAX_LINE_LENGTH {
                tracing::warn!("Oversized line from {}, dropping connection", peer_addr);
                break;
            }

            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            tracing::debug!("Received from {}: {} bytes", peer_addr, line.len());

            let request: AcpRequest = match serde_json::from_str(line) {
                Ok(req) => req,
                Err(e) => {
                    let response = AcpResponse::error(None, -32700, &format!("Parse error: {}", e));
                    send_response(&writer, &response).await?;
                    continue;
                }
            };

            let method = request.method.clone();

            if method == "prompt" {
                // Streaming: spawn task with channel
                let handler = self.handler.clone();
                let writer = writer.clone();
                let (tx, rx) = mpsc::channel::<String>(32);

                let auth_clone = auth.clone();
                tokio::spawn(async move {
                    let writer_clone = writer.clone();
                    let notify_task = tokio::spawn(async move {
                        let mut rx = rx;
                        while let Some(notification) = rx.recv().await {
                            let mut w = writer_clone.lock().await;
                            if let Err(e) = w.write_all(notification.as_bytes()).await {
                                tracing::error!("Failed to write notification: {}", e);
                                break;
                            }
                            if let Err(_) = w.write_all(b"\n").await {
                                break;
                            }
                            if let Err(_) = w.flush().await {
                                break;
                            }
                        }
                    });

                    let response = handler.handle_prompt(request, tx, auth_clone).await;

                    if response.result.as_ref().and_then(|r| r.get("streaming")).is_none() {
                        if let Err(e) = send_response(&writer, &response).await {
                            tracing::error!("Failed to send prompt error: {}", e);
                        }
                    }

                    let _ = notify_task.await;
                });
            } else {
                // Non-streaming
                let (response, new_auth) = self.handler.handle_request(request, auth.clone()).await;
                // Update auth state if handler returned a new one
                if let Some(new_auth_state) = new_auth {
                    auth = Some(new_auth_state);
                }
                send_response(&writer, &response).await?;
            }
        }

        Ok(())
    }
}

async fn send_response(
    writer: &Arc<tokio::sync::Mutex<BufWriter<tokio::net::tcp::OwnedWriteHalf>>>,
    response: &AcpResponse,
) -> anyhow::Result<()> {
    let json = response.to_ndjson()?;
    let mut w = writer.lock().await;
    w.write_all(json.as_bytes()).await?;
    w.write_all(b"\n").await?;
    w.flush().await?;
    Ok(())
}
