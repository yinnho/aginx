//! Agent management for aginx

mod manager;
mod permission;
mod session;

pub use manager::{AgentInfo, AgentManager};
pub use permission::{PermissionManager, PermissionOption, PermissionRequest, PermissionRequestPreview, PermissionResponse};
pub use session::{SessionConfig, SessionManager, SessionInfo, SendMessageResult, PermissionPrompt, PermissionOption as SessionPermissionOption};
