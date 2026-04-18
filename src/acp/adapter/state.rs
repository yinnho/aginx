//! Adapter and session lifecycle state definitions.

use serde::{Deserialize, Serialize};

/// Adapter connection lifecycle states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterState {
    /// Agent process is starting up / initializing
    Initializing,
    /// Connection is alive and ready to accept requests
    Ready,
    /// Currently processing a prompt
    Busy,
    /// Connection encountered an error
    Error,
    /// Adapter is shutting down
    ShuttingDown,
}

impl std::fmt::Display for AdapterState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AdapterState::Initializing => write!(f, "initializing"),
            AdapterState::Ready => write!(f, "ready"),
            AdapterState::Busy => write!(f, "busy"),
            AdapterState::Error => write!(f, "error"),
            AdapterState::ShuttingDown => write!(f, "shutting_down"),
        }
    }
}

/// Session lifecycle states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    /// Session has been created (session/new completed)
    Created,
    /// A prompt is currently being processed
    Active,
    /// Prompt completed, waiting for next prompt
    Idle,
    /// Session has been closed
    Closed,
}

impl std::fmt::Display for SessionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionState::Created => write!(f, "created"),
            SessionState::Active => write!(f, "active"),
            SessionState::Idle => write!(f, "idle"),
            SessionState::Closed => write!(f, "closed"),
        }
    }
}
