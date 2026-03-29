//! Request handler for aginx TCP server (ACP Protocol)
//!
//! This handler implements the ACP (Agent Client Protocol) over TCP.
//! ACP uses JSON-RPC 2.0 with ndjson (newline-delimited JSON) format.
//!
//! CONCURRENT MODEL: `prompt` requests are spawned as independent tasks so
//! the main loop can continue reading `permissionResponse` without deadlock.

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
        // Get agents directory from config
        let agents_dir = config.agents.get_agents_dir();
        tracing::info!("Agents directory: {}", agents_dir.display());

        let agent_manager = Arc::new(agent_manager);

        // Create ACP handler using the same managers
        let acp_handler = Arc::new(AcpHandler::with_agents_dir(
            agent_manager.clone(),
            session_manager.clone(),
            agents_dir
        ));

        Self {
            config,
            agent_manager,
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
                    send_response(&writer, &response).await?;
                    continue;
                }
            };

            let method = request.method.clone();

            if method == "prompt" {
                // SPAWN: run streaming in a separate task so the main loop
                // can continue reading the next request (e.g. permissionResponse)
                let acp_handler = self.acp_handler.clone();
                let writer = writer.clone();
                let (tx, rx) = mpsc::channel::<String>(32);

                tokio::spawn(async move {
                    // Notification forwarder: writes streaming notifications to TCP
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

                    // Run the streaming prompt
                    let response = acp_handler.handle_prompt_streaming(request, tx).await;

                    // Wait for all notifications to be written
                    let _ = notify_task.await;

                    // Send final response
                    if let Ok(json) = response.to_ndjson() {
                        tracing::trace!("ACP prompt response: {}", json);
                        let mut w = writer.lock().await;
                        let _ = write_ndjson(&mut *w, &json).await;
                    }
                });

                // Main loop continues immediately - does NOT wait for prompt to finish
            } else {
                // Non-streaming methods: handle synchronously
                let response = self.acp_handler.handle_request(request).await;
                send_response(&writer, &response).await?;
            }
        }

        Ok(())
    }
}

/// Send ACP response (standalone function, no &self needed)
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
