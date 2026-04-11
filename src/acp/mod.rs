//! ACP (Agent Client Protocol) implementation for aginx
//!
//! This module implements the ACP protocol for IDE/Client integration.
//! ACP uses ndjson (newline-delimited JSON) over stdin/stdout.

mod types;
mod handler;
mod notifications;
mod streaming;
pub mod agent_process;
pub mod agent_event;
pub mod adapter;

#[cfg(feature = "acp-native")]
mod backend;
#[cfg(feature = "acp-native")]
mod acp_backend;

#[cfg(test)]
mod tests;

pub use types::*;
pub use handler::*;

/// 连接认证状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionAuth {
    /// 未认证，只能用 public agent + bindDevice
    Pending,
    /// 已认证，全功能可用
    Authenticated,
}
