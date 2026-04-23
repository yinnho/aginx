//! Authentication module
//!
//! Two-level auth:
//! - Binding: exclusive, full permissions (device pairing)
//! - Authorization: multiple clients, restricted permissions (JWT-based)

pub mod jwt;
pub mod manager;

pub use jwt::*;
pub use manager::*;

/// Auth level after successful authentication
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthLevel {
    /// Bound device — full permissions
    Bound,
    /// Authorized client — restricted by JWT claims
    Authorized(AuthorizedClient),
}
