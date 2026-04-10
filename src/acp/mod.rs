//! ACP (Agent Client Protocol) implementation for aginx
//!
//! This module implements the ACP protocol for IDE/Client integration.
//! ACP uses ndjson (newline-delimited JSON) over stdin/stdout.

mod types;
mod handler;
mod streaming;
pub mod agent_process;
pub mod agent_event;

#[cfg(test)]
mod tests;

pub use types::*;
pub use handler::*;
