//! Agent management for aginx

mod discovery;
pub mod ledger;
mod manager;
pub mod setup;

pub use manager::{AgentInfo, AgentManager};
