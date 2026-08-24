//! Simplified protocol for aginx
//!
//! aginx uses minimal JSON-RPC 2.0:
//! Client → prompt → aginx spawns CLI → streams stdout → final result

mod types;
mod handler;
pub mod adapter;
#[cfg(test)]
mod golden;

pub use types::*;
pub use handler::*;
