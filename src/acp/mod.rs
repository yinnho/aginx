//! ACP (Agent Client Protocol) implementation for aginx
//!
//! This module implements the ACP protocol for IDE/Client integration.
//! ACP uses ndjson (newline-delimited JSON) over stdin/stdout.

mod types;
mod handler;
mod stream;
mod streaming;

#[cfg(test)]
mod tests;

pub use types::*;
pub use handler::*;
pub use stream::*;
pub use streaming::*;
