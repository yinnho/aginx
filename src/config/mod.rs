//! Configuration management

mod loader;

pub use loader::*;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Get the aginx data directory (~/.aginx)
pub fn data_dir() -> PathBuf {
    dirs::home_dir()
        .map(|h| h.join(".aginx"))
        .unwrap_or_else(|| PathBuf::from(".aginx"))
}

/// Get the aginx agents directory (~/.aginx/agents)
pub fn agents_dir() -> PathBuf {
    data_dir().join("agents")
}

/// Server mode
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ServerMode {
    Direct,
    #[default]
    Relay,
}

/// Default relay domain
pub const DEFAULT_RELAY_DOMAIN: &str = "relay.yinnho.cn";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum AccessMode {
    /// Fully open — no authentication required
    Public,
    /// Team/internal — JWT or device binding (flexible)
    Protected,
    /// Personal — strict device pairing required
    #[default]
    Private,
}

/// Main config
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub api: ApiConfig,
    #[serde(default)]
    pub relay: RelayConfig,
    #[serde(default)]
    pub direct: DirectConfig,
    #[serde(default)]
    pub auth: AuthConfig,
    #[serde(default)]
    pub agents: AgentsConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: ServerConfig::default(),
            api: ApiConfig::default(),
            relay: RelayConfig::default(),
            direct: DirectConfig::default(),
            auth: AuthConfig::default(),
            agents: AgentsConfig::default(),
        }
    }
}

impl Config {
    /// Validate config values — returns Err for critical issues
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.server.port == 0 {
            anyhow::bail!("server.port must be > 0");
        }
        if self.server.max_connections == 0 {
            anyhow::bail!("server.max_connections must be > 0");
        }
        Ok(())
    }
}

/// Server config
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    #[serde(default)]
    pub mode: ServerMode,
    #[serde(default = "default_server_name")]
    pub name: String,
    #[serde(default = "default_server_version")]
    pub version: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default)]
    pub access: AccessMode,
    #[serde(default = "default_max_connections")]
    pub max_connections: usize,
    #[serde(default = "default_max_concurrent_sessions")]
    pub max_concurrent_sessions: usize,
    #[serde(default = "default_session_timeout_seconds")]
    pub session_timeout_seconds: u64,
}

fn default_server_name() -> String { "aginx".to_string() }
fn default_server_version() -> String { "0.1.0".to_string() }
fn default_port() -> u16 { 86 }
fn default_host() -> String { "0.0.0.0".to_string() }
fn default_max_connections() -> usize { 100 }
fn default_max_concurrent_sessions() -> usize { 10 }
fn default_session_timeout_seconds() -> u64 { 1800 }

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            mode: ServerMode::default(),
            name: default_server_name(),
            version: default_server_version(),
            port: default_port(),
            host: default_host(),
            access: AccessMode::default(),
            max_connections: default_max_connections(),
            max_concurrent_sessions: default_max_concurrent_sessions(),
            session_timeout_seconds: default_session_timeout_seconds(),
        }
    }
}

/// Default relay TLS port
pub const DEFAULT_RELAY_PORT: u16 = 8443;

/// Relay config
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayConfig {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default = "default_relay_domain")]
    pub domain: String,
    #[serde(default = "default_relay_port")]
    pub port: u16,
    #[serde(default = "default_true")]
    pub use_tls: bool,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default = "default_heartbeat_interval")]
    pub heartbeat_interval: u64,
    #[serde(default = "default_reconnect_interval")]
    pub reconnect_interval: u64,
    #[serde(default)]
    pub publish_agents: bool,
    /// Shared secret for relay authentication (optional; if set, relay requires it)
    #[serde(default)]
    pub relay_secret: Option<String>,
}

fn default_relay_domain() -> String { DEFAULT_RELAY_DOMAIN.to_string() }
fn default_relay_port() -> u16 { DEFAULT_RELAY_PORT }
fn default_true() -> bool { true }
fn default_heartbeat_interval() -> u64 { 30 }
fn default_reconnect_interval() -> u64 { 5 }

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            id: None,
            token: None,
            domain: default_relay_domain(),
            port: default_relay_port(),
            use_tls: default_true(),
            url: None,
            heartbeat_interval: default_heartbeat_interval(),
            reconnect_interval: default_reconnect_interval(),
            publish_agents: false,
            relay_secret: None,
        }
    }
}

impl RelayConfig {
    pub fn get_connect_url(&self) -> String {
        if let Some(ref url) = self.url {
            url.clone()
        } else {
            format!("{}:{}", self.domain, self.port)
        }
    }

    pub fn has_id(&self) -> bool {
        self.id.is_some()
    }

    pub fn set_id(&mut self, id: String) {
        self.id = Some(id.clone());
        self.url = Some(format!("{}.{}:{}", id, self.domain, self.port));
    }
}

/// Direct mode config
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectConfig {
    #[serde(default)]
    pub public_url: Option<String>,
}

impl Default for DirectConfig {
    fn default() -> Self {
        Self { public_url: None }
    }
}

/// API config
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiConfig {
    #[serde(default = "default_api_url")]
    pub url: String,
}

fn default_api_url() -> String { "https://api.yinnho.cn".to_string() }

impl Default for ApiConfig {
    fn default() -> Self {
        Self { url: default_api_url() }
    }
}

/// Auth config
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    pub jwt_secret: Option<String>,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self { jwt_secret: None }
    }
}

/// Agents config
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentsConfig {
    #[serde(default)]
    pub list: Vec<AgentEntry>,
    #[serde(default)]
    pub dir: Option<PathBuf>,
}

impl AgentsConfig {
    pub fn get_agents_dir(&self) -> PathBuf {
        self.dir.clone().unwrap_or_else(agents_dir)
    }
}

impl Default for AgentsConfig {
    fn default() -> Self {
        Self { list: vec![], dir: None }
    }
}

fn default_agent_type() -> String { "process".to_string() }

/// Agent entry in main config
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEntry {
    pub id: String,
    pub name: String,
    #[serde(default = "default_agent_type")]
    pub agent_type: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub env: std::collections::HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
}
