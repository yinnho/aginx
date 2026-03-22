//! Protocol types for aginx
//!
//! JSON-RPC 2.0 based protocol implementation.

mod jsonrpc;
mod message;

pub use jsonrpc::*;
pub use message::*;

/// Protocol version
pub const PROTOCOL_VERSION: &str = "0.1.0";

/// Default port
pub const DEFAULT_PORT: u16 = 86;
