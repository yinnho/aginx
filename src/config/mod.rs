//! 配置管理

mod loader;

pub use loader::*;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 运行模式
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ServerMode {
    /// 直连模式 - 监听本地 TCP 86
    #[default]
    Direct,
    /// 中继模式 - 连接 relay 服务器
    Relay,
}

/// 访问模式
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AccessMode {
    /// 公开模式 - 任何人可访问
    Public,
    /// 专属模式 - 需要认证
    #[default]
    Private,
}

/// 主配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// 服务器配置
    #[serde(default)]
    pub server: ServerConfig,

    /// 中继配置
    #[serde(default)]
    pub relay: RelayConfig,

    /// 直连配置
    #[serde(default)]
    pub direct: DirectConfig,

    /// 认证配置
    #[serde(default)]
    pub auth: AuthConfig,

    /// Agent 配置
    #[serde(default)]
    pub agents: AgentsConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: ServerConfig::default(),
            relay: RelayConfig::default(),
            direct: DirectConfig::default(),
            auth: AuthConfig::default(),
            agents: AgentsConfig::default(),
        }
    }
}

/// 服务器配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    /// 运行模式: direct(直连) | relay(中继)
    #[serde(default)]
    pub mode: ServerMode,

    /// 服务名称
    #[serde(default = "default_server_name")]
    pub name: String,

    /// 服务版本
    #[serde(default = "default_server_version")]
    pub version: String,

    /// 本地服务端口 (两种模式都需要，本地调用)
    #[serde(default = "default_port")]
    pub port: u16,

    /// 绑定地址
    #[serde(default = "default_host")]
    pub host: String,

    /// 访问模式
    #[serde(default)]
    pub access: AccessMode,

    /// 最大连接数
    #[serde(default = "default_max_connections")]
    pub max_connections: usize,
}

fn default_server_name() -> String { "aginx".to_string() }
fn default_server_version() -> String { "0.1.0".to_string() }
fn default_port() -> u16 { 86 }
fn default_host() -> String { "0.0.0.0".to_string() }
fn default_max_connections() -> usize { 100 }

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
        }
    }
}

/// 中继配置 (mode = "relay" 时使用)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayConfig {
    /// Relay 完整地址
    /// 格式: {aginx_id}.relay.yinnho.cn:8600
    /// 例如: abc123.relay.yinnho.cn:8600
    #[serde(default)]
    pub url: Option<String>,

    /// 心跳间隔 (秒)
    #[serde(default = "default_heartbeat_interval")]
    pub heartbeat_interval: u64,

    /// 重连间隔 (秒)
    #[serde(default = "default_reconnect_interval")]
    pub reconnect_interval: u64,
}

fn default_heartbeat_interval() -> u64 { 30 }
fn default_reconnect_interval() -> u64 { 5 }

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            url: None,
            heartbeat_interval: default_heartbeat_interval(),
            reconnect_interval: default_reconnect_interval(),
        }
    }
}

/// 直连配置 (mode = "direct" 时使用)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectConfig {
    /// 公网访问地址
    /// 例如: agent://myserver.com
    #[serde(default)]
    pub public_url: Option<String>,
}

impl Default for DirectConfig {
    fn default() -> Self {
        Self {
            public_url: None,
        }
    }
}

/// 认证配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    /// JWT 密钥
    pub jwt_secret: Option<String>,

    /// 允许的用户
    #[serde(default)]
    pub users: Vec<UserConfig>,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            jwt_secret: None,
            users: Vec::new(),
        }
    }
}

/// 用户配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserConfig {
    pub id: String,
    pub name: String,
}

/// Agent 配置
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentsConfig {
    /// 内置 agents
    #[serde(default)]
    pub builtin: Vec<BuiltinAgentConfig>,

    /// 进程 agents
    #[serde(default)]
    pub process: Vec<ProcessAgentConfig>,
}

/// 内置 Agent 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuiltinAgentConfig {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
}

/// 进程 Agent 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessAgentConfig {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub working_dir: Option<PathBuf>,
    pub env: Option<std::collections::HashMap<String, String>>,
}
