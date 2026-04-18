//! End-to-end encryption for relay connections.
//!
//! Uses X25519 ECDH for key exchange and ChaCha20-Poly1305 for encryption.
//! The relay server only sees encrypted bytes — it cannot read messages.
//!
//! Handshake:
//! 1. Daemon generates a static X25519 keypair on startup
//! 2. Public key is sent to client during device binding (out-of-band)
//! 3. Client generates an ephemeral keypair, derives shared secret via ECDH
//! 4. Client sends `e2ee_hello` with its public key
//! 5. Daemon derives the same shared secret, responds `e2ee_ready`
//! 6. All subsequent messages are encrypted with ChaCha20-Poly1305

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::Mutex;
use x25519_dalek::{PublicKey, StaticSecret};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use chacha20poly1305::aead::{Aead, KeyInit};

/// Daemon-side E2EE session manager.
/// Holds the daemon's static keypair and per-client shared secrets.
pub struct E2eeSession {
    /// Daemon's static secret key
    secret: StaticSecret,
    /// Daemon's public key (sent to clients out-of-band)
    public_key: PublicKey,
    /// Per-client encryption state: client_id → PeerSession
    peers: Arc<Mutex<HashMap<String, PeerSession>>>,
}

struct PeerSession {
    /// ChaCha20-Poly1305 cipher derived from the shared secret
    cipher: ChaCha20Poly1305,
    /// Transmit nonce counter (daemon → client)
    tx_nonce: AtomicU64,
    /// Receive nonce counter (client → daemon)
    rx_nonce: AtomicU64,
}

impl E2eeSession {
    /// Create a new E2EE session with a fresh random keypair.
    pub fn new() -> Self {
        let secret = StaticSecret::new(rand::rngs::OsRng);
        let public_key = PublicKey::from(&secret);
        Self {
            secret,
            public_key,
            peers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Get the daemon's public key as bytes.
    pub fn public_key_bytes(&self) -> &[u8; 32] {
        self.public_key.as_bytes()
    }

    /// Get the daemon's public key as base64 string.
    pub fn public_key_b64(&self) -> String {
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, self.public_key.as_bytes())
    }

    /// Handle an e2ee_hello from a client: derive shared secret.
    /// `client_public_b64` is the client's ephemeral public key, base64-encoded.
    pub async fn handle_hello(&self, client_id: &str, client_public_b64: &str) -> Result<(), String> {
        let client_public_bytes: [u8; 32] = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, client_public_b64)
            .map_err(|e| format!("Invalid client public key: {}", e))?
            .try_into()
            .map_err(|_| "Client public key must be 32 bytes")?;

        let client_public = PublicKey::from(client_public_bytes);
        let shared_secret = self.secret.diffie_hellman(&client_public);

        // Derive a 32-byte key from the shared secret
        let key_material = shared_secret.as_bytes();
        let cipher = ChaCha20Poly1305::new(Key::from_slice(key_material));

        self.peers.lock().await.insert(client_id.to_string(), PeerSession {
            cipher,
            tx_nonce: AtomicU64::new(0),
            rx_nonce: AtomicU64::new(0),
        });

        tracing::info!("E2EE handshake completed for client {}", client_id);
        Ok(())
    }

    /// Check if a client has an established E2EE session.
    pub async fn has_peer(&self, client_id: &str) -> bool {
        self.peers.lock().await.contains_key(client_id)
    }

    /// Encrypt a plaintext message for a client.
    pub async fn encrypt(&self, client_id: &str, plaintext: &[u8]) -> Result<Vec<u8>, String> {
        let peers = self.peers.lock().await;
        let peer = peers.get(client_id)
            .ok_or_else(|| format!("No E2EE session for client {}", client_id))?;

        let nonce_val = peer.tx_nonce.fetch_add(1, Ordering::Relaxed);
        let nonce = nonce_to_bytes(nonce_val);

        peer.cipher.encrypt(&nonce, plaintext)
            .map_err(|e| format!("Encryption failed: {}", e))
    }

    /// Decrypt a ciphertext from a client.
    pub async fn decrypt(&self, client_id: &str, ciphertext: &[u8]) -> Result<Vec<u8>, String> {
        let peers = self.peers.lock().await;
        let peer = peers.get(client_id)
            .ok_or_else(|| format!("No E2EE session for client {}", client_id))?;

        let nonce_val = peer.rx_nonce.fetch_add(1, Ordering::Relaxed);
        let nonce = nonce_to_bytes(nonce_val);

        peer.cipher.decrypt(&nonce, ciphertext)
            .map_err(|e| format!("Decryption failed: {}", e))
    }

    /// Remove a client's E2EE session (on disconnect).
    pub async fn remove_peer(&self, client_id: &str) {
        self.peers.lock().await.remove(client_id);
    }
}

/// Convert a u64 nonce counter to a 12-byte ChaCha20-Poly1305 nonce.
fn nonce_to_bytes(counter: u64) -> Nonce {
    let mut nonce = [0u8; 12];
    nonce[4..12].copy_from_slice(&counter.to_le_bytes());
    Nonce::clone(Nonce::from_slice(&nonce))
}
