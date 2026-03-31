//! Agent management for aginx

mod discovery;
mod manager;
mod permission;
mod session;

pub use discovery::{AgentConfig, scan_directory, parse_aginx_toml};
pub use manager::{AgentInfo, AgentManager};
pub use permission::{PermissionManager, PermissionOption, PermissionRequest, PermissionRequestPreview, PermissionResponse};
pub use session::{SessionConfig, SessionManager};
