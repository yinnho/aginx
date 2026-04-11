//! Re-export facade for backward compatibility during migration
//!
//! Claude 专用事件类型已移至 adapter/claude/events.rs
//! 通用通知构建函数已移至 notifications.rs

// Claude 专用事件类型
pub(crate) use super::adapter::claude::events::{
    AgentEvent, AssistantMessage, ContentBlock, StreamDelta, StreamInnerEvent,
};

// 通用通知构建函数
pub use super::notifications::{
    write_ndjson, make_chunk_notification, make_tool_notification,
    make_tool_update_notification, format_tool_title, infer_tool_kind,
    stop_reason_str, truncate_preview,
};
