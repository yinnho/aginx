//! AgentBackend trait — aginx 与 Agent 通信的统一接口
//!
//! 所有 agent 通过此 trait 接入 aginx：
//! - ACP 原生 agent（如 opencarrier）→ AcpBackend（透传）
//! - Claude CLI → ClaudeStreamAdapter（adapter 翻译）

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc;

/// Agent 通信后端接口
#[async_trait]
pub trait AgentBackend: Send + Sync {
    /// 创建新会话，返回 agent 侧的 session ID
    async fn create_session(
        &self,
        session_id: &str,
        agent_id: &str,
        workdir: Option<&str>,
    ) -> Result<String>;

    /// 加载/恢复已有会话
    async fn load_session(
        &self,
        session_id: &str,
        agent_session_id: Option<&str>,
        workdir: Option<&str>,
    ) -> Result<()>;

    /// 发送 prompt，通过 tx 发送 ACP sessionUpdate 通知（JSON 字符串）
    /// 完成时发送最终响应
    async fn prompt(
        &self,
        session_id: &str,
        message: &str,
        tx: mpsc::Sender<String>,
    ) -> Result<()>;

    /// 取消正在进行的 prompt
    async fn cancel(&self, session_id: &str) -> Result<()>;

    /// 关闭会话并释放资源
    async fn close_session(&self, session_id: &str) -> Result<()>;

    /// 列出对话
    async fn list_conversations(&self, agent_id: &str) -> Result<Vec<Value>>;

    /// 获取对话消息
    async fn get_messages(
        &self,
        session_id: &str,
        agent_id: &str,
        limit: u32,
    ) -> Result<Vec<Value>>;

    /// 删除对话
    async fn delete_conversation(&self, session_id: &str, agent_id: &str) -> Result<()>;
}
