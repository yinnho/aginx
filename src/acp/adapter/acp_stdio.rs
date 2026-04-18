//! Persistent ACP stdio adapter — pure ACP message forwarder.
//!
//! Aginx is a router (like nginx). This adapter transparently forwards ACP
//! JSON-RPC messages to the agent process. No session management, no ID mapping,
//! no agent-specific logic.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

use crate::acp::adapter::PromptAdapter;
use crate::acp::types::{AcpResponse, Id, PromptResult, StopReason};
use crate::agent::AgentInfo;

static REQUEST_COUNTER: AtomicU64 = AtomicU64::new(1);

fn next_request_id() -> u64 {
    REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// Maintains the long-running stdio connection to an ACP agent.
struct AgentConnection {
    child: tokio::process::Child,
    writer: tokio::io::BufWriter<tokio::process::ChildStdin>,
    stdout_rx: mpsc::Receiver<String>,
    /// Capabilities extracted from initialize response
    cached_capabilities: Option<serde_json::Value>,
}

impl AgentConnection {
    fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for AgentConnection {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

/// ACP stdio adapter — speaks standard ACP JSON-RPC over stdin/stdout.
/// Pure passthrough: no session management, no ID mapping, no agent-specific logic.
pub struct AcpStdioAdapter {
    command: String,
    args: Vec<String>,
    env: std::collections::HashMap<String, String>,
    timeout_secs: u64,
    agent_id: String,
    connection: Arc<tokio::sync::Mutex<Option<AgentConnection>>>,
    /// Current adapter lifecycle state
    state: Arc<tokio::sync::Mutex<super::state::AdapterState>>,
    /// Capabilities cached from initialize handshake
    cached_capabilities: Arc<tokio::sync::Mutex<Option<serde_json::Value>>>,
    /// Session storage path from config (for filesystem scan fallback)
    storage_path: Option<String>,
    /// Session storage format from config (e.g. "copilot-events", "gemini-json")
    storage_format: Option<String>,
}

impl AcpStdioAdapter {
    pub fn new(agent_info: &AgentInfo) -> Self {
        Self {
            command: agent_info.command.clone(),
            args: agent_info.args.clone(),
            env: agent_info.env.clone(),
            timeout_secs: agent_info.timeout.unwrap_or(300),
            agent_id: agent_info.id.clone(),
            connection: Arc::new(tokio::sync::Mutex::new(None)),
            state: Arc::new(tokio::sync::Mutex::new(super::state::AdapterState::Ready)),
            cached_capabilities: Arc::new(tokio::sync::Mutex::new(None)),
            storage_path: agent_info.storage_path.clone(),
            storage_format: agent_info.storage_format.clone(),
        }
    }

    /// Ensure the persistent connection is alive, respawning if necessary.
    async fn ensure_conn(
        &self,
    ) -> Result<tokio::sync::MutexGuard<'_, Option<AgentConnection>>, String> {
        let mut guard = self.connection.lock().await;
        let needs_respawn = match guard.as_mut() {
            Some(conn) => !conn.is_alive(),
            None => true,
        };
        if needs_respawn {
            if let Some(conn) = guard.take() {
                drop(conn);
            }
            *self.state.lock().await = super::state::AdapterState::Initializing;
            match spawn_and_init_agent(&self.command, &self.args, &self.env).await {
                Ok(conn) => {
                    *self.cached_capabilities.lock().await = conn.cached_capabilities.clone();
                    *guard = Some(conn);
                    *self.state.lock().await = super::state::AdapterState::Ready;
                }
                Err(e) => {
                    *self.state.lock().await = super::state::AdapterState::Error;
                    return Err(e);
                }
            }
        }
        Ok(guard)
    }
}

#[async_trait::async_trait]
impl PromptAdapter for AcpStdioAdapter {
    async fn send_request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let mut guard = self.ensure_conn().await?;
        let conn = guard.as_mut().ok_or("Agent connection unavailable")?;
        let id = next_request_id();
        write_request(&mut conn.writer, id, method, params).await?;
        let resp = read_response(&mut conn.stdout_rx, id).await?;
        if let Some(error) = resp.get("error") {
            let msg = error.get("message").and_then(|m| m.as_str()).unwrap_or("Agent error");
            return Err(msg.to_string());
        }
        Ok(resp.get("result").cloned().unwrap_or(serde_json::json!({})))
    }

    async fn send_streaming_request(
        &self,
        method: &str,
        params: serde_json::Value,
        tx: Option<mpsc::Sender<String>>,
    ) -> Result<serde_json::Value, String> {
        let mut guard = self.ensure_conn().await?;
        let conn = guard.as_mut().ok_or("Agent connection unavailable")?;
        let id = next_request_id();
        write_request(&mut conn.writer, id, method, params).await?;
        // Read response, forwarding notifications to tx if provided
        let resp = read_response_with_forward(&mut conn.stdout_rx, id, tx.as_ref()).await?;
        if let Some(error) = resp.get("error") {
            let msg = error.get("message").and_then(|m| m.as_str()).unwrap_or("Agent error");
            return Err(msg.to_string());
        }
        Ok(resp.get("result").cloned().unwrap_or(serde_json::json!({})))
    }

    async fn prompt(
        &self,
        session_id: &str,
        message: &str,
        request_id: Option<Id>,
        tx: mpsc::Sender<String>,
    ) -> Result<(), String> {
        let tx = crate::acp::timeline::EventSender::new(tx);
        let session_id = session_id.to_string();
        let message = message.to_string();
        let timeout_secs = self.timeout_secs;
        let connection = self.connection.clone();
        let state = self.state.clone();

        tokio::spawn(async move {
            let result: Result<(), String> = async {
                let mut guard = connection.lock().await;
                let conn = guard.as_mut().ok_or("Agent connection unavailable")?;
                *state.lock().await = super::state::AdapterState::Busy;

                let prompt_id = next_request_id();
                write_request(&mut conn.writer, prompt_id, "session/prompt", serde_json::json!({
                    "sessionId": session_id,
                    "prompt": [{"type": "text", "text": &message}],
                })).await?;

                let result = tokio::time::timeout(
                    std::time::Duration::from_secs(timeout_secs),
                    read_prompt_response(&mut conn.stdout_rx, prompt_id, &session_id, request_id.clone(), tx.clone()),
                ).await;

                match result {
                    Ok(Ok(())) => Ok(()),
                    Ok(Err(e)) => {
                        let err = AcpResponse::error(request_id, -32603, &format!("Agent error: {}", e));
                        let _ = tx.send_response(&err).await;
                        Ok(())
                    }
                    Err(_) => {
                        tracing::warn!("Agent timed out after {}s", timeout_secs);
                        let err = AcpResponse::error(request_id, -32603, &format!("Agent timed out after {}s", timeout_secs));
                        let _ = tx.send_response(&err).await;
                        Ok(())
                    }
                }
            }.await;

            if let Err(e) = result {
                tracing::error!("ACP stdio error: {}", e);
                *state.lock().await = super::state::AdapterState::Error;
                let mut guard = connection.lock().await;
                if let Some(conn) = guard.take() {
                    drop(conn);
                }
            } else {
                *state.lock().await = super::state::AdapterState::Ready;
            }
        });

        Ok(())
    }

    async fn list_sessions(&self) -> Result<Vec<serde_json::Value>, String> {
        // Path 1: try standard ACP session/list
        if let Ok(mut guard) = self.ensure_conn().await {
            if let Some(conn) = guard.as_mut() {
                let req_id = next_request_id();
                if write_request(&mut conn.writer, req_id, "session/list", serde_json::json!({})).await.is_ok() {
                    if let Ok(resp) = read_response(&mut conn.stdout_rx, req_id).await {
                        if resp.get("error").is_none() {
                            // Standard ACP returns { sessions: [...] }
                            let mut sessions = resp.get("result")
                                .and_then(|r| r.get("sessions"))
                                .and_then(|s| s.as_array())
                                .cloned()
                                .unwrap_or_default();

                            // Log raw first session for debugging
                            if let Some(first) = sessions.first() {
                                tracing::info!("[ACP] agent={} raw session sample: {}", self.agent_id,
                                    serde_json::to_string(first).unwrap_or_default());
                            }

                            // Normalize field names and types
                            for session in &mut sessions {
                                session["agentId"] = serde_json::Value::String(self.agent_id.clone());
                                // Map id → sessionId if missing
                                if session.get("sessionId").is_none() {
                                    if let Some(id) = session.get("id").cloned() {
                                        session["sessionId"] = id;
                                    }
                                }
                                // Map summary → title if missing
                                if session.get("title").is_none() {
                                    if let Some(summary) = session.get("summary").cloned() {
                                        session["title"] = summary;
                                    }
                                }
                                // Map cwd → workdir if missing
                                if session.get("workdir").is_none() {
                                    if let Some(cwd) = session.get("cwd").cloned() {
                                        session["workdir"] = cwd;
                                    }
                                }
                                // Convert ISO timestamps to milliseconds
                                if let Some(ts) = session.get("createdAt").and_then(|v| v.as_str()) {
                                    if let Some(ms) = super::session_scan::parse_iso_timestamp(ts) {
                                        session["createdAt"] = serde_json::Value::Number(ms.into());
                                    }
                                }
                                if let Some(ts) = session.get("updatedAt").and_then(|v| v.as_str()) {
                                    if let Some(ms) = super::session_scan::parse_iso_timestamp(ts) {
                                        session["updatedAt"] = serde_json::Value::Number(ms.into());
                                    }
                                }
                                // Also try created_at / updated_at (snake_case variants)
                                if session.get("createdAt").is_none() {
                                    if let Some(ts) = session.get("created_at").and_then(|v| v.as_str()) {
                                        if let Some(ms) = super::session_scan::parse_iso_timestamp(ts) {
                                            session["createdAt"] = serde_json::Value::Number(ms.into());
                                        }
                                    }
                                }
                                if session.get("updatedAt").is_none() {
                                    if let Some(ts) = session.get("updated_at").and_then(|v| v.as_str()) {
                                        if let Some(ms) = super::session_scan::parse_iso_timestamp(ts) {
                                            session["updatedAt"] = serde_json::Value::Number(ms.into());
                                        }
                                    }
                                }
                            }
                            tracing::info!("[ACP] agent={} session/list returned {} sessions", self.agent_id, sessions.len());
                            return Ok(sessions);
                        } else {
                            tracing::info!("[ACP] agent={} session/list error: {:?}", self.agent_id, resp.get("error"));
                        }
                    } else {
                        tracing::info!("[ACP] agent={} session/list read_response failed", self.agent_id);
                    }
                } else {
                    tracing::info!("[ACP] agent={} session/list write_request failed", self.agent_id);
                }
            } else {
                tracing::info!("[ACP] agent={} ensure_conn returned None", self.agent_id);
            }
        } else {
            tracing::info!("[ACP] agent={} ensure_conn failed", self.agent_id);
        }

        // Path 2: config-driven filesystem scan
        if let (Some(ref path), Some(ref fmt)) = (&self.storage_path, &self.storage_format) {
            let sessions = super::session_scan::scan_sessions(path, fmt, &self.agent_id);
            if !sessions.is_empty() {
                return Ok(sessions);
            }
        }

        Ok(Vec::new())
    }

    async fn get_messages(&self, session_id: &str, limit: usize) -> Result<Vec<serde_json::Value>, String> {
        // Path 1: try ACP _aginx/getMessages
        if let Ok(mut guard) = self.ensure_conn().await {
            if let Some(conn) = guard.as_mut() {
                let req_id = next_request_id();
                if write_request(
                    &mut conn.writer,
                    req_id,
                    "_aginx/getMessages",
                    serde_json::json!({ "conversationId": session_id, "limit": limit }),
                ).await.is_ok() {
                    if let Ok(resp) = read_response(&mut conn.stdout_rx, req_id).await {
                        if resp.get("error").is_none() {
                            let messages = resp.get("result")
                                .and_then(|r| r.get("messages"))
                                .and_then(|m| m.as_array())
                                .cloned()
                                .unwrap_or_default();
                            return Ok(messages);
                        }
                    }
                }
            }
        }

        // Path 2: config-driven filesystem scan
        if let (Some(ref path), Some(ref fmt)) = (&self.storage_path, &self.storage_format) {
            let messages = super::session_scan::read_messages(session_id, limit, path, fmt);
            if !messages.is_empty() {
                return Ok(messages);
            }
        }

        Ok(Vec::new())
    }

    async fn delete_session(&self, session_id: &str) -> Result<bool, String> {
        let mut guard = self.ensure_conn().await?;
        let conn = guard.as_mut().ok_or("Agent connection unavailable")?;

        let req_id = next_request_id();
        write_request(
            &mut conn.writer,
            req_id,
            "_aginx/deleteConversation",
            serde_json::json!({
                "conversationId": session_id,
            }),
        )
        .await?;

        let resp = read_response(&mut conn.stdout_rx, req_id).await?;
        if let Some(error) = resp.get("error") {
            let msg = error.get("message").and_then(|m| m.as_str()).unwrap_or("deleteConversation failed");
            return Err(msg.to_string());
        }

        let deleted = resp
            .get("result")
            .and_then(|r| r.get("deleted"))
            .and_then(|d| d.as_bool())
            .unwrap_or(true);

        Ok(deleted)
    }

    fn adapter_state(&self) -> super::state::AdapterState {
        match self.state.try_lock() {
            Ok(guard) => *guard,
            Err(_) => super::state::AdapterState::Ready,
        }
    }

    fn cached_capabilities(&self) -> Option<serde_json::Value> {
        self.cached_capabilities.try_lock().ok().and_then(|guard| guard.clone())
    }
}

/// Spawn the agent process, start stdout reader, and complete initialize handshake.
async fn spawn_and_init_agent(
    command: &str,
    args: &[String],
    env: &std::collections::HashMap<String, String>,
) -> Result<AgentConnection, String> {
    let mut cmd = tokio::process::Command::new(command);
    cmd.args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .process_group(0);

    for (k, v) in env {
        cmd.env(k, v);
    }

    let mut child = cmd.spawn()
        .map_err(|e| format!("Failed to spawn {}: {}", command, e))?;

    let stdin = child.stdin.take().ok_or("Failed to get stdin")?;
    let stdout = child.stdout.take().ok_or("Failed to get stdout")?;
    let stderr = child.stderr.take();

    if let Some(stderr) = stderr {
        tokio::spawn(async move {
            let reader = tokio::io::BufReader::new(stderr);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!("Agent stderr: {}", line);
            }
        });
    }

    let mut writer = tokio::io::BufWriter::new(stdin);
    let (stdout_tx, mut stdout_rx) = mpsc::channel::<String>(64);
    tokio::spawn(async move {
        let reader = tokio::io::BufReader::new(stdout);
        let mut lines = reader.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let line = line.trim().to_string();
            if !line.is_empty() {
                if stdout_tx.send(line).await.is_err() {
                    break;
                }
            }
        }
    });

    let init_id = next_request_id();
    write_request(&mut writer, init_id, "initialize", serde_json::json!({
        "protocolVersion": 1,
        "clientInfo": {"name": "aginx", "version": env!("CARGO_PKG_VERSION")},
    })).await?;

    let init_resp = read_response(&mut stdout_rx, init_id).await?;
    if let Some(error) = init_resp.get("error") {
        let msg = error.get("message").and_then(|m| m.as_str()).unwrap_or("Init failed");
        return Err(format!("Agent init failed: {}", msg));
    }

    tracing::debug!("Agent initialized: {}", init_resp.get("result")
        .and_then(|r| r.get("agentInfo"))
        .and_then(|a| a.get("name"))
        .and_then(|n| n.as_str())
        .unwrap_or("unknown"));

    let cached_capabilities = init_resp.get("result")
        .and_then(|r| r.get("agentCapabilities"))
        .cloned();

    Ok(AgentConnection { child, writer, stdout_rx, cached_capabilities })
}

/// Send a JSON-RPC request to the agent's stdin
async fn write_request(
    writer: &mut tokio::io::BufWriter<tokio::process::ChildStdin>,
    id: u64,
    method: &str,
    params: serde_json::Value,
) -> Result<(), String> {
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    });
    let line = serde_json::to_string(&request).map_err(|e| format!("JSON encode: {}", e))?;
    writer.write_all(line.as_bytes()).await.map_err(|e| format!("Write: {}", e))?;
    writer.write_all(b"\n").await.map_err(|e| format!("Write: {}", e))?;
    writer.flush().await.map_err(|e| format!("Flush: {}", e))?;
    Ok(())
}

/// Read a JSON-RPC response with a matching id from the stdout channel.
/// Skips any notifications (messages without the expected id).
async fn read_response(
    rx: &mut mpsc::Receiver<String>,
    expected_id: u64,
) -> Result<serde_json::Value, String> {
    while let Some(line) = rx.recv().await {
        let msg: serde_json::Value = match serde_json::from_str(&line) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if msg.get("id").and_then(|v| v.as_u64()) == Some(expected_id) {
            return Ok(msg);
        }
    }
    Err("Agent closed stdout before response".to_string())
}

/// Read a JSON-RPC response, forwarding intermediate notifications to tx.
async fn read_response_with_forward(
    rx: &mut mpsc::Receiver<String>,
    expected_id: u64,
    tx: Option<&mpsc::Sender<String>>,
) -> Result<serde_json::Value, String> {
    while let Some(line) = rx.recv().await {
        let msg: serde_json::Value = match serde_json::from_str(&line) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if msg.get("id").and_then(|v| v.as_u64()) == Some(expected_id) {
            return Ok(msg);
        }
        // Forward notifications to client
        if let Some(sender) = tx {
            if sender.send(line).await.is_err() {
                return Err("Client disconnected".to_string());
            }
        }
    }
    Err("Agent closed stdout before response".to_string())
}

/// Read prompt response: transparently forward notifications, collect final response.
async fn read_prompt_response(
    stdout_rx: &mut mpsc::Receiver<String>,
    prompt_id: u64,
    session_id: &str,
    request_id: Option<Id>,
    tx: crate::acp::timeline::EventSender,
) -> Result<(), String> {
    while let Some(line) = stdout_rx.recv().await {
        let msg: serde_json::Value = match serde_json::from_str(&line) {
            Ok(m) => m,
            Err(_) => continue,
        };

        tracing::debug!("[ACP-STDIO] msg: id={:?} method={:?} update={:?}",
            msg.get("id"),
            msg.get("method"),
            msg.get("params").and_then(|p| p.get("update")).and_then(|u| u.get("sessionUpdate")),
        );

        let msg_id = msg.get("id").and_then(|v| v.as_u64());

        if msg_id == Some(prompt_id) {
            // Final response from agent
            if let Some(error) = msg.get("error") {
                let err_msg = error.get("message").and_then(|m| m.as_str()).unwrap_or("Agent error");
                return Err(err_msg.to_string());
            }

            let stop_reason_str = msg.get("result")
                .and_then(|r| r.get("stopReason"))
                .and_then(|s| s.as_str())
                .unwrap_or("end_turn");

            let stop_reason = match stop_reason_str {
                "end_turn" => StopReason::EndTurn,
                "max_tokens" => StopReason::MaxTokens,
                "max_turn_requests" => StopReason::MaxTurnRequests,
                "refusal" => StopReason::Refusal,
                "cancelled" => StopReason::Cancelled,
                _ => StopReason::EndTurn,
            };

            let final_resp = AcpResponse::success(request_id, PromptResult { stopReason: stop_reason });
            let _ = tx.send_response(&final_resp).await;
            return Ok(());
        } else if msg.get("method").is_some() {
            // Notification — transparently forward to client via TimelineEvent
            if let Some(event) = crate::acp::timeline::TimelineEvent::from_agent_notification(&msg, session_id) {
                if tx.send_event(event).await.is_err() {
                    tracing::warn!("Client disconnected");
                    return Ok(());
                }
            }
        }
    }

    Err("Agent closed stdout without final response".to_string())
}
