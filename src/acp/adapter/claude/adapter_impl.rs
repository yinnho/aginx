//! Claude adapter implementation — wraps streaming.rs for legacy Claude CLI

use std::sync::Arc;
use tokio::sync::mpsc;

use crate::acp::adapter::PromptAdapter;
use crate::acp::streaming_types::{AsyncStreamingResult, StreamingEvent};
use crate::acp::types::{AcpResponse, Id, PromptResult};
use crate::acp::notifications::truncate_preview;
use crate::agent::{AgentInfo, SessionManager};

/// Claude adapter — handles prompt execution for Claude CLI (legacy stream-json protocol)
pub struct ClaudeAdapter {
    agent_info: AgentInfo,
    session_manager: Arc<SessionManager>,
}

impl ClaudeAdapter {
    pub fn new_legacy(agent_info: AgentInfo, session_manager: Arc<SessionManager>) -> Self {
        Self { agent_info, session_manager }
    }
}

#[async_trait::async_trait]
impl PromptAdapter for ClaudeAdapter {
    async fn send_request(
        &self,
        _method: &str,
        _params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        Err("Claude adapter does not support ACP passthrough".to_string())
    }

    async fn send_streaming_request(
        &self,
        _method: &str,
        _params: serde_json::Value,
        _tx: Option<mpsc::Sender<String>>,
    ) -> Result<serde_json::Value, String> {
        Err("Claude adapter does not support ACP passthrough".to_string())
    }

    async fn prompt(
        &self,
        session_id: &str,
        message: &str,
        request_id: Option<Id>,
        tx: mpsc::Sender<String>,
    ) -> Result<(), String> {
        prompt_legacy(session_id, &self.agent_info.id, message, request_id, tx, &self.agent_info, &self.session_manager).await
    }

    async fn list_sessions(&self) -> Result<Vec<serde_json::Value>, String> {
        let storage_path = self.agent_info.storage_path.as_deref();
        let sessions = if let Some(path) = storage_path {
            let expanded = shellexpand::tilde(path).to_string();
            let dir = std::path::Path::new(&expanded);
            super::session_storage::scan_claude_sessions(dir, &self.agent_info.id)
        } else {
            Vec::new()
        };
        Ok(sessions)
    }

    async fn get_messages(&self, session_id: &str, limit: usize) -> Result<Vec<serde_json::Value>, String> {
        let messages = super::session_storage::read_jsonl_messages_limited(
            session_id,
            limit,
            self.agent_info.storage_path.as_deref(),
        );
        Ok(messages)
    }

    async fn delete_session(&self, session_id: &str) -> Result<bool, String> {
        let deleted = super::session_storage::delete_jsonl_by_session_id(
            session_id,
            self.agent_info.storage_path.as_deref(),
        );
        Ok(deleted)
    }
}

/// Legacy streaming: spawn new Claude CLI process per prompt
async fn prompt_legacy(
    session_id: &str,
    agent_id: &str,
    message: &str,
    request_id: Option<Id>,
    tx: mpsc::Sender<String>,
    agent_info: &AgentInfo,
    session_manager: &Arc<SessionManager>,
) -> Result<(), String> {
    let session_info = session_manager.get_session_info(session_id).await
        .ok_or_else(|| format!("Session not found: {}", session_id))?;

    let mut event_rx = super::streaming::StreamingSession::run_streaming_async(
        session_id.to_string(),
        agent_info.clone(),
        session_info.workdir.clone(),
        session_info.agent_session_id.clone(),
        message,
    ).await?;

    // Save user message
    session_manager.append_message(session_id, agent_id, "user", message);

    let sm = session_manager.clone();
    let sid = session_id.to_string();
    let aid = agent_id.to_string();
    let rid = request_id;

    tokio::spawn(async move {
        let mut final_result: Option<AsyncStreamingResult> = None;

        while let Some(event) = event_rx.recv().await {
            match event {
                StreamingEvent::Notification(json) => {
                    if tx.send(json).await.is_err() {
                        tracing::warn!("Client disconnected");
                        break;
                    }
                }
                StreamingEvent::Completed(result) => {
                    final_result = Some(result);
                    break;
                }
            }
        }

        if let Some(result) = final_result {
            match result {
                AsyncStreamingResult::Completed { stop_reason, content, agent_session_id } => {
                    if let Some(ref sid_inner) = agent_session_id {
                        if let Err(e) = sm.update_agent_session_id(&sid, sid_inner).await {
                            tracing::warn!("Failed to save agent_session_id: {}", e);
                        }
                    }
                    let msg_preview = truncate_preview(&content, 100);
                    sm.update_persisted_metadata(&sid, &aid, agent_session_id.as_deref(), Some(&msg_preview));
                    sm.append_message(&sid, &aid, "assistant", &content);

                    let final_response = AcpResponse::success(rid, PromptResult { stopReason: stop_reason });
                    let _ = tx.send(serde_json::to_string(&final_response).unwrap_or_default()).await;
                }
                AsyncStreamingResult::Error(e) => {
                    tracing::error!("Streaming error: {}", e);
                    let error_response = AcpResponse::error(rid, -32603, &format!("Streaming error: {}", e));
                    let _ = tx.send(serde_json::to_string(&error_response).unwrap_or_default()).await;
                }
            }
        }
    });

    Ok(())
}
