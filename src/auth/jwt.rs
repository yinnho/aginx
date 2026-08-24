//! JWT authentication for authorized client connections

use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};

/// JWT claims for authorized clients
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthClientClaims {
    /// Client ID
    pub sub: String,
    /// Client name
    pub name: String,
    /// Allowed agent IDs (empty = all)
    pub agents: Vec<String>,
    /// Allowed methods (empty = all)
    pub methods: Vec<String>,
    /// Allow system methods (listDirectory, readFile, etc.)
    pub sys: bool,
    /// Issued at
    pub iat: i64,
    /// Expiration (unix timestamp)
    pub exp: i64,
    /// Audience
    pub aud: String,
}

/// Verify an authorized client JWT.
/// Returns Ok(claims) on success.
pub fn verify_auth_client_jwt(token: &str, jwt_secret: &str) -> Result<AuthClientClaims, String> {
    let mut validation = Validation::default();
    validation.set_audience(&["aginx-auth"]);
    let token_data = decode::<AuthClientClaims>(
        token,
        &DecodingKey::from_secret(jwt_secret.as_bytes()),
        &validation,
    )
    .map_err(|e| format!("Invalid auth JWT: {}", e))?;

    Ok(token_data.claims)
}

/// Generate an authorized client JWT.
pub fn generate_auth_client_jwt(
    claims: &AuthClientClaims,
    jwt_secret: &str,
) -> Result<String, String> {
    encode(
        &Header::default(),
        claims,
        &EncodingKey::from_secret(jwt_secret.as_bytes()),
    )
    .map_err(|e| format!("Failed to encode JWT: {}", e))
}
