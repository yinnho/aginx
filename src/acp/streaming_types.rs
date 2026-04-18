//! 通用流式类型 — adapter 和 handler 共用

use super::types::StopReason;

/// 异步流式结果（通过 channel 发送）
#[derive(Debug, Clone)]
pub enum AsyncStreamingResult {
    /// 成功完成
    Completed {
        stop_reason: StopReason,
        content: String,
        agent_session_id: Option<String>,
    },
    /// 出错
    Error(String),
}

/// 流式事件（用于异步 channel 通信）
#[derive(Debug, Clone)]
pub enum StreamingEvent {
    /// ACP session/update 通知 JSON
    Notification(String),
    /// 流式完成
    Completed(AsyncStreamingResult),
}
