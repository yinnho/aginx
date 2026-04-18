//! Session management (simplified)
//!
//! Generates session IDs for prompt tracking. No persistence needed
//! since each prompt spawns a fresh CLI process.

use uuid::Uuid;

/// Generate a new session ID
pub fn new_session_id() -> String {
    Uuid::new_v4().to_string()
}

/// Session config (kept for compatibility)
#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub max_concurrent: usize,
    pub timeout_seconds: u64,
}

/// Session manager stub (kept for compatibility)
pub struct SessionManager;

impl SessionManager {
    pub fn new(_config: SessionConfig) -> Self {
        Self
    }
}
