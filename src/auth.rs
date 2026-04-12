//! JWT authentication for aginx-to-aginx connections
//!
//! Verifies JWT tokens signed by aginx-api, allowing registered
//! aginx instances to authenticate with each other.

use jsonwebtoken::{decode, DecodingKey, Validation};
use serde::Deserialize;

/// JWT claims (mirrors aginx-api auth::Claims)
#[derive(Debug, Deserialize)]
struct Claims {
    /// aginx instance ID
    sub: String,
    /// fingerprint hash
    #[allow(dead_code)]
    fp: String,
    /// Expiration (unix timestamp) — validated by jsonwebtoken::Validation
    #[allow(dead_code)]
    exp: usize,
}

/// Verify a JWT token against the shared jwt_secret.
/// Returns Ok(aginx_id) on success, Err on invalid/expired token.
pub fn verify_jwt(token: &str, jwt_secret: &str) -> Result<String, String> {
    let token_data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(jwt_secret.as_bytes()),
        &Validation::default(),
    )
    .map_err(|e| format!("Invalid JWT: {}", e))?;

    let aginx_id = token_data.claims.sub;
    tracing::info!("JWT verified for aginx_id={}", aginx_id);
    Ok(aginx_id)
}
