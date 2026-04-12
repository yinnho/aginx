//! ACP Client — lightweight client for aginx-to-aginx communication
//!
//! Allows one aginx instance to connect to another aginx and call its Agents.
//! Uses the same ACP protocol (JSON-RPC 2.0 over ndjson) as regular clients.

#![allow(dead_code)]

use std::sync::atomic::{AtomicU64, Ordering};

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, BufWriter};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use super::types::{AcpRequest, AcpResponse, Id};

/// Lightweight ACP client for connecting to another aginx
pub struct AcpClient {
    writer: BufWriter<Box<dyn AsyncWrite + Unpin + Send>>,
    reader: BufReader<Box<dyn AsyncRead + Unpin + Send>>,
    next_id: AtomicU64,
}

impl AcpClient {
    /// Connect to a remote aginx via direct TCP
    pub async fn connect(addr: &str, jwt_token: &str) -> anyhow::Result<Self> {
        let stream = TcpStream::connect(addr).await?;
        let (reader, writer) = stream.into_split();

        let mut client = Self {
            writer: BufWriter::new(Box::new(writer)),
            reader: BufReader::new(Box::new(reader)),
            next_id: AtomicU64::new(1),
        };

        client.initialize(jwt_token).await?;
        tracing::info!("AcpClient connected and authenticated to {}", addr);
        Ok(client)
    }

    /// Connect via relay (TCP + optional TLS)
    pub async fn connect_via_relay(relay_addr: &str, target_id: &str, jwt_token: &str, use_tls: bool) -> anyhow::Result<Self> {
        let tcp_stream = TcpStream::connect(relay_addr).await?;

        let (reader, writer): (Box<dyn AsyncRead + Unpin + Send>, Box<dyn AsyncWrite + Unpin + Send>) = if use_tls {
            let domain = relay_addr.split(':').next().unwrap_or(relay_addr);
            let tls_connector = native_tls::TlsConnector::new()?;
            let tls_stream = tokio_native_tls::TlsConnector::from(tls_connector)
                .connect(domain, tcp_stream)
                .await?;
            let (r, w) = tokio::io::split(tls_stream);
            (Box::new(r), Box::new(w))
        } else {
            let (r, w) = tcp_stream.into_split();
            (Box::new(r), Box::new(w))
        };

        let mut client = Self {
            writer: BufWriter::new(writer),
            reader: BufReader::new(reader),
            next_id: AtomicU64::new(1),
        };

        // Send relay connect message
        let connect_msg = serde_json::json!({
            "type": "connect",
            "target": target_id
        });
        client.send_raw(&serde_json::to_string(&connect_msg)?).await?;

        // Read connect response
        let mut line = String::new();
        client.reader.read_line(&mut line).await?;
        let resp: serde_json::Value = serde_json::from_str(line.trim())?;
        if resp.get("type").and_then(|t| t.as_str()) == Some("error") {
            let msg = resp.get("message").and_then(|m| m.as_str()).unwrap_or("unknown");
            return Err(anyhow::anyhow!("Relay connect failed: {}", msg));
        }

        // ACP initialize with JWT
        client.initialize(jwt_token).await?;
        tracing::info!("AcpClient connected via relay to {}", target_id);
        Ok(client)
    }

    /// List agents on the remote aginx
    pub async fn list_agents(&mut self) -> anyhow::Result<Vec<serde_json::Value>> {
        let req = self.build_request("listAgents", serde_json::json!({}));
        self.send_request(&req).await?;
        let resp = self.read_response().await?;
        self.extract_array(resp, "agents")
    }

    /// Create a new session with an agent
    pub async fn create_session(&mut self, agent_id: &str, workdir: Option<&str>) -> anyhow::Result<String> {
        let req = self.build_request("session/new", serde_json::json!({
            "cwd": workdir,
            "_meta": {"agentId": agent_id}
        }));
        self.send_request(&req).await?;
        let resp = self.read_response().await?;
        self.extract_string(resp, "sessionId")
    }

    /// Load an existing session
    pub async fn load_session(&mut self, session_id: &str, agent_id: Option<&str>) -> anyhow::Result<String> {
        let mut params = serde_json::json!({"sessionId": session_id});
        if let Some(aid) = agent_id {
            params["agentId"] = serde_json::json!(aid);
        }
        let req = self.build_request("session/load", params);
        self.send_request(&req).await?;
        let resp = self.read_response().await?;
        self.extract_string(resp, "sessionId")
    }

    /// Send a prompt to a session, returning a channel receiver for streaming events.
    /// Caller reads PromptEvent values from the receiver.
    ///
    /// Note: consumes self because the background reader task takes ownership of the
    /// reader half. For multiple prompts on the same session, create a new AcpClient.
    pub async fn prompt(mut self, session_id: &str, message: &str) -> anyhow::Result<mpsc::Receiver<PromptEvent>> {
        let req = self.build_request("session/prompt", serde_json::json!({
            "sessionId": session_id,
            "prompt": [{"type": "text", "text": message}]
        }));
        self.send_request(&req).await?;

        // Read initial response (should be {"streaming": true, ...})
        let initial = self.read_response().await?;
        if let Some(error) = &initial.error {
            return Err(anyhow::anyhow!("Prompt failed: {}", error.message));
        }

        let (tx, rx) = mpsc::channel::<PromptEvent>(64);

        // Spawn background reader task
        tokio::spawn(async move {
            let mut reader = self.reader;

            // Forward initial response if not streaming
            let streaming = initial.result.as_ref()
                .and_then(|r| r.get("streaming").and_then(|v| v.as_bool()))
                .unwrap_or(false);

            if !streaming {
                // Non-streaming: send the result as a Done event
                let content = initial.result.as_ref()
                    .and_then(|r| r.get("response"))
                    .and_then(|v| v.as_str())
                    .map(String::from)
                    .unwrap_or_default();
                let _ = tx.send(PromptEvent::Done { content }).await;
                return;
            }

            // Read streaming lines
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => break,
                    Ok(_) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() { continue; }

                        if let Ok(resp) = serde_json::from_str::<AcpResponse>(trimmed) {
                            if resp.error.is_some() {
                                let msg = resp.error.as_ref()
                                    .map(|e| e.message.clone())
                                    .unwrap_or_default();
                                let _ = tx.send(PromptEvent::Error { message: msg }).await;
                                break;
                            }
                            // Final response with result
                            if resp.result.is_some() {
                                let content = resp.result.as_ref()
                                    .and_then(|r| r.get("response"))
                                    .and_then(|v| v.as_str())
                                    .map(String::from)
                                    .unwrap_or_default();
                                let _ = tx.send(PromptEvent::Done { content }).await;
                                break;
                            }
                        } else {
                            // Likely a notification — forward as raw event
                            let _ = tx.send(PromptEvent::Notification(trimmed.to_string())).await;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(rx)
    }

    /// Close a session
    pub async fn close_session(&mut self, session_id: &str) -> anyhow::Result<()> {
        let req = self.build_request("session/cancel", serde_json::json!({
            "sessionId": session_id
        }));
        self.send_request(&req).await?;
        let _ = self.read_response().await?;
        Ok(())
    }

    // === Internal helpers ===

    async fn initialize(&mut self, jwt_token: &str) -> anyhow::Result<()> {
        let init_req = AcpRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(Id::Number(1)),
            method: "initialize".to_string(),
            params: Some(serde_json::json!({
                "protocolVersion": "0.15.0",
                "clientInfo": {"name": "aginx", "version": env!("CARGO_PKG_VERSION")},
                "_meta": {"authToken": jwt_token}
            })),
        };
        self.send_request(&init_req).await?;

        let resp = self.read_response().await?;
        if let Some(error) = resp.error {
            return Err(anyhow::anyhow!("Initialize failed: {}", error.message));
        }

        let authenticated = resp.result
            .as_ref()
            .and_then(|r| r.get("authenticated").and_then(|v| v.as_bool()))
            .unwrap_or(false);
        if !authenticated {
            return Err(anyhow::anyhow!("Authentication failed: server did not authenticate"));
        }

        Ok(())
    }

    fn next_request_id(&self) -> i64 {
        self.next_id.fetch_add(1, Ordering::Relaxed) as i64
    }

    fn build_request(&self, method: &str, params: serde_json::Value) -> AcpRequest {
        AcpRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(Id::Number(self.next_request_id())),
            method: method.to_string(),
            params: Some(params),
        }
    }

    async fn send_request(&mut self, request: &AcpRequest) -> anyhow::Result<()> {
        let json = serde_json::to_string(request)?;
        self.send_raw(&json).await
    }

    async fn send_raw(&mut self, line: &str) -> anyhow::Result<()> {
        self.writer.write_all(format!("{}\n", line).as_bytes()).await?;
        self.writer.flush().await?;
        Ok(())
    }

    async fn read_response(&mut self) -> anyhow::Result<AcpResponse> {
        let mut line = String::new();
        let n = self.reader.read_line(&mut line).await?;
        if n == 0 {
            return Err(anyhow::anyhow!("Connection closed"));
        }
        let resp: AcpResponse = serde_json::from_str(line.trim())?;
        Ok(resp)
    }

    fn extract_string(&self, resp: AcpResponse, field: &str) -> anyhow::Result<String> {
        if let Some(error) = resp.error {
            return Err(anyhow::anyhow!("Error: {}", error.message));
        }
        resp.result
            .and_then(|r| r.get(field).and_then(|v| v.as_str()).map(String::from))
            .ok_or_else(|| anyhow::anyhow!("Missing '{}' in response", field))
    }

    fn extract_array(&self, resp: AcpResponse, field: &str) -> anyhow::Result<Vec<serde_json::Value>> {
        if let Some(error) = resp.error {
            return Err(anyhow::anyhow!("Error: {}", error.message));
        }
        resp.result
            .and_then(|r| r.get(field).and_then(|v| v.as_array()).cloned())
            .ok_or_else(|| anyhow::anyhow!("Missing '{}' in response", field))
    }
}

/// Events from a streaming prompt response
#[derive(Debug, Clone)]
pub enum PromptEvent {
    /// A notification (sessionUpdate, etc.)
    Notification(String),
    /// Final response with content
    Done { content: String },
    /// Error occurred
    Error { message: String },
}
