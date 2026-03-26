//! Request handler for aginx TCP server (ACP Protocol)
//!
//! This handler implements the ACP (Agent Client Protocol) over TCP.
//! ACP uses JSON-RPC 2.0 with ndjson (newline-delimited JSON) format.

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use crate::config::Config;
use crate::agent::{AgentManager, SessionManager};
use crate::binding::BindingManager;
use crate::acp::{AcpHandler, AcpRequest, AcpResponse};

/// Helper: write a line as NDJSON (with newline) and flush
async fn write_ndjson<W: AsyncWriteExt + Unpin>(writer: &mut W, line: &str) -> std::io::Result<()> {
    writer.write_all(line.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

/// Request handler
pub struct Handler {
    config: Arc<Config>,
    agent_manager: Arc<AgentManager>,
    binding_manager: Arc<tokio::sync::Mutex<BindingManager>>,
    session_manager: Arc<SessionManager>,
    acp_handler: Arc<AcpHandler>,
}

impl Handler {
    /// Create a new handler
    pub fn new(
        config: Arc<Config>,
        agent_manager: AgentManager,
        binding_manager: Arc<tokio::sync::Mutex<BindingManager>>,
        session_manager: Arc<SessionManager>,
    ) -> Self {
        // Create ACP handler using the same managers
        let acp_handler = Arc::new(AcpHandler::new(
            Arc::new(agent_manager.clone()),
            session_manager.clone()
        ));

        Self {
            config,
            agent_manager: Arc::new(agent_manager),
            binding_manager,
            session_manager,
            acp_handler
        }
    }

    /// Handle the connection
    pub async fn handle(self, stream: TcpStream, peer_addr: SocketAddr) -> anyhow::Result<()> {
        tracing::info!("ACP connection from {}", peer_addr);

        let (reader, writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let writer = Arc::new(tokio::sync::Mutex::new(BufWriter::new(writer)));
        let mut line = String::new();

        loop {
            line.clear();

            // Read a line (ACP request in ndjson format)
            let bytes_read = reader.read_line(&mut line).await?;
            if bytes_read == 0 {
                tracing::debug!("ACP connection closed by {}", peer_addr);
                break;
            }

            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            tracing::trace!("ACP received: {}", line);

            // Parse ACP request
            let request: AcpRequest = match serde_json::from_str(line) {
                Ok(req) => req,
                Err(e) => {
                    tracing::error!("Failed to parse ACP request: {}", e);
                    let response = AcpResponse::error(None, -32700, &format!("Parse error: {}", e));
                    self.send_acp_response(&writer, &response).await?;
                    continue;
                }
            };

            // Handle request with ACP handler
            let method = request.method.clone();
            let request_id = request.id.clone();

            // Handle streaming methods (prompt, permissionResponse)
            if method == "prompt" || method == "permissionResponse" {
                // Create channel for notifications
                let (tx, mut rx) = mpsc::channel::<String>(32);
                let writer_clone = writer.clone();

                // Spawn task to write notifications
                let notify_task = tokio::spawn(async move {
                    while let Some(notification) = rx.recv().await {
                        let mut w = writer_clone.lock().await;
                        if let Err(e) = write_ndjson(&mut *w, &notification).await {
                            tracing::error!("Failed to write notification: {}", e);
                            break;
                        }
                    }
                });

                // Handle with streaming
                let response = if method == "prompt" {
                    self.acp_handler.handle_prompt_streaming(request, tx).await
                } else {
                    self.acp_handler.handle_permission_response_streaming(request, tx).await
                };

                // Wait for notification task to complete
                let _ = notify_task.await;

                // Send final response
                self.send_acp_response(&writer, &response).await?;
            } else {
                // Non-streaming methods
                let response = self.acp_handler.handle_request(request).await;
                self.send_acp_response(&writer, &response).await?;
            }
        }

        Ok(())
    }

    /// Send ACP response
    async fn send_acp_response(
        &self,
        writer: &Arc<tokio::sync::Mutex<BufWriter<tokio::net::tcp::OwnedWriteHalf>>>,
        response: &AcpResponse
    ) -> anyhow::Result<()> {
        let json = response.to_ndjson()?;
        tracing::trace!("ACP response: {}", json);
        let mut w = writer.lock().await;
        write_ndjson(&mut *w, &json).await?;
        Ok(())
    }
}
