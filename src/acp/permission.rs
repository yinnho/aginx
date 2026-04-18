//! Permission routing for aginx.
//!
//! Manages the complete permission request/response flow:
//! Agent → aginx (intercept) → Client → User → aginx → Agent
//!
//! Supports auto-approve policies for specific tool types.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;

/// Tracks pending permission requests across all sessions.
pub struct PermissionRouter {
    pending: Arc<Mutex<HashMap<String, PendingPermission>>>,
}

/// A pending permission request awaiting user response.
pub struct PendingPermission {
    /// The agent that requested permission
    pub agent_id: String,
    /// The agent-side session ID
    pub session_id: String,
    /// The aginx-side session ID
    pub aginx_session_id: String,
    /// When the request was created
    pub created_at: Instant,
}

/// Configuration for automatic permission approval.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct PermissionPolicy {
    /// Tool names that are automatically approved (e.g., "Read", "Glob", "Grep")
    #[serde(default)]
    pub default_allowed: Vec<String>,
    /// Auto-approve all requests (dangerous, testing only)
    #[serde(default)]
    pub auto_approve_all: bool,
}

impl PermissionRouter {
    pub fn new() -> Self {
        Self {
            pending: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Register a pending permission request.
    pub async fn register(
        &self,
        request_id: String,
        agent_id: String,
        session_id: String,
        aginx_session_id: String,
    ) {
        self.pending.lock().await.insert(request_id, PendingPermission {
            agent_id,
            session_id,
            aginx_session_id,
            created_at: Instant::now(),
        });
    }

    /// Look up and remove a pending permission request.
    pub async fn take(&self, request_id: &str) -> Option<PendingPermission> {
        self.pending.lock().await.remove(request_id)
    }

    /// Check if a request ID is pending.
    pub async fn is_pending(&self, request_id: &str) -> bool {
        self.pending.lock().await.contains_key(request_id)
    }
}

impl PermissionPolicy {
    /// Check if a tool should be automatically approved.
    pub fn should_auto_approve(&self, tool_name: &str) -> bool {
        if self.auto_approve_all {
            return true;
        }
        self.default_allowed.iter().any(|allowed| {
            tool_name.eq_ignore_ascii_case(allowed)
        })
    }
}
