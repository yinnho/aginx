//! Agent management for aginx

mod discovery;
mod manager;
mod session;
pub mod setup;

pub use discovery::{scan_directory, parse_aginx_toml, agent_config_to_info};
pub use manager::{AgentInfo, AgentManager};
pub use session::{SessionConfig, SessionManager, WorkspaceSummary};
