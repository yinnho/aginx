//! ACP (Agent Client Protocol) implementation for aginx
//!
//! This module implements the ACP protocol for IDE/Client integration.
//! ACP uses ndjson (newline-delimited JSON) over stdin/stdout.

mod types;
mod handler;
pub mod notifications;
pub mod streaming_types;
pub mod adapter;
pub mod acp_client;
pub mod timeline;
pub mod permission;
pub mod orchestrator;

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
