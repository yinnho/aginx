//! Authorization manager for multi-client access
//!
//! Stores authorized clients in auth.json with per-client permissions.
//! Unlike binding (exclusive), authorization supports multiple clients.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};

/// Global authorization manager
static AUTH_MANAGER: Lazy<Arc<Mutex<AuthManager>>> = Lazy::new(|| {
    Arc::new(Mutex::new(AuthManager::new_internal()))
});

/// Get global auth manager
pub fn get_auth_manager() -> Arc<Mutex<AuthManager>> {
    AUTH_MANAGER.clone()
}

/// Authorized client info (stored in auth.json)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthorizedClient {
    pub id: String,
    pub name: String,
    pub token: String,
    pub created_at: i64,
    pub expires_at: Option<i64>,
    pub allowed_agents: Vec<String>,
    pub allowed_methods: Vec<String>,
    pub allow_system: bool,
}

/// Authorization manager
pub struct AuthManager {
    data_dir: PathBuf,
    clients: HashMap<String, AuthorizedClient>,
}

impl AuthManager {
    fn new_internal() -> Self {
        let data_dir = crate::config::data_dir();
        let clients = Self::load_clients(&data_dir);
        Self { data_dir, clients }
    }

    fn clients_path(&self) -> PathBuf {
        self.data_dir.join("auth.json")
    }

    fn load_clients(data_dir: &Path) -> HashMap<String, AuthorizedClient> {
        let path = data_dir.join("auth.json");
        if !path.exists() {
            return HashMap::new();
        }
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("Failed to read auth.json: {}", e);
                return HashMap::new();
            }
        };
        let list: Vec<AuthorizedClient> = match serde_json::from_str(&content) {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!("Failed to parse auth.json: {}", e);
                return HashMap::new();
            }
        };
        list.into_iter().map(|c| (c.id.clone(), c)).collect()
    }

    fn save_clients(&self) -> anyhow::Result<()> {
        let path = self.clients_path();
        let list: Vec<&AuthorizedClient> = self.clients.values().collect();
        let content = serde_json::to_string_pretty(&list)?;
        crate::binding::write_secret_file(&path, &content)?;
        Ok(())
    }

    /// Add a new authorized client
    pub fn add_client(&mut self, client: AuthorizedClient) -> anyhow::Result<()> {
        self.clients.insert(client.id.clone(), client);
        self.save_clients()?;
        Ok(())
    }

    /// Remove an authorized client by ID
    pub fn remove_client(&mut self, id: &str) -> bool {
        let removed = self.clients.remove(id).is_some();
        if removed {
            if let Err(e) = self.save_clients() {
                tracing::warn!("Failed to save auth.json after removal: {}", e);
            }
        }
        removed
    }

    /// Find client by token (constant-time comparison)
    pub fn find_by_token(&self, token: &str) -> Option<&AuthorizedClient> {
        self.clients.values().find(|c| {
            crate::binding::constant_time_eq(&c.token, token)
        })
    }

    /// List all authorized clients
    pub fn list_clients(&self) -> Vec<&AuthorizedClient> {
        self.clients.values().collect()
    }

    /// Clean expired clients
    pub fn clean_expired(&mut self) -> usize {
        let now = chrono::Utc::now().timestamp();
        let before = self.clients.len();
        self.clients.retain(|_, c| {
            c.expires_at.map(|exp| now < exp).unwrap_or(true)
        });
        let removed = before - self.clients.len();
        if removed > 0 {
            if let Err(e) = self.save_clients() {
                tracing::warn!("Failed to save auth.json after cleanup: {}", e);
            }
        }
        removed
    }
}
