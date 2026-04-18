pub mod claude;
pub mod process;
pub mod acp_stdio;
pub mod session_scan;
pub mod state;

use std::sync::Arc;
use tokio::sync::mpsc;

use crate::agent::AgentInfo;
use crate::agent::SessionManager;

/// Agent 适配器 — 纯路由转发 ACP 消息
#[async_trait::async_trait]
pub trait PromptAdapter: Send + Sync {
    /// 发送任意 ACP 请求给 agent，返回 agent 的 JSON-RPC 响应。
    /// 用于 session/new, session/cancel, session/list 等非流式方法。
    async fn send_request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String>;

    /// 发送流式 ACP 请求（如 session/load），转发中间通知，返回最终响应。
    async fn send_streaming_request(
        &self,
        method: &str,
        params: serde_json::Value,
        tx: Option<mpsc::Sender<String>>,
    ) -> Result<serde_json::Value, String>;

    /// 发送 session/prompt 并流式转发 session/update 通知。
    async fn prompt(
        &self,
        session_id: &str,
        message: &str,
        request_id: Option<super::types::Id>,
        tx: mpsc::Sender<String>,
    ) -> Result<(), String>;

    /// 列出该 agent 的会话
    async fn list_sessions(&self) -> Result<Vec<serde_json::Value>, String> {
        Ok(Vec::new())
    }

    /// 获取指定会话的消息历史
    async fn get_messages(&self, _session_id: &str, _limit: usize) -> Result<Vec<serde_json::Value>, String> {
        Ok(Vec::new())
    }

    /// 删除指定会话
    async fn delete_session(&self, _session_id: &str) -> Result<bool, String> {
        Ok(false)
    }

    /// 获取 adapter 当前连接状态
    fn adapter_state(&self) -> state::AdapterState {
        state::AdapterState::Ready
    }

    /// 获取 initialize 握手时缓存的能力信息
    fn cached_capabilities(&self) -> Option<serde_json::Value> {
        None
    }
}

/// 根据 agent 配置创建对应的 adapter
pub fn create_adapter(
    agent_info: &AgentInfo,
    session_manager: Arc<SessionManager>,
) -> Arc<dyn PromptAdapter> {
    match agent_info.protocol.as_str() {
        "claude-stream" => {
            Arc::new(claude::ClaudeAdapter::new_legacy(agent_info.clone(), session_manager))
        }
        "acp" => {
            Arc::new(acp_stdio::AcpStdioAdapter::new(agent_info))
        }
        _ => {
            // 通用 stdin/stdout 进程模式
            Arc::new(process::ProcessAdapter::new(agent_info))
        }
    }
}
