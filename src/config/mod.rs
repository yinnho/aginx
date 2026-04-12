//! 配置管理

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

/// 运行模式
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ServerMode {
    /// 直连模式 - 监听本地 TCP 86
    Direct,
    /// 中继模式 - 连接 relay 服务器（默认）
    #[default]
    Relay,
}

/// 默认 Relay 域名
pub const DEFAULT_RELAY_DOMAIN: &str = "relay.yinnho.cn";
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq)]
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

    /// API 配置
    #[serde(default)]
    pub api: ApiConfig,

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
            api: ApiConfig::default(),
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

    /// 最大并发会话数
    #[serde(default = "default_max_concurrent_sessions")]
    pub max_concurrent_sessions: usize,

    /// 会话超时（秒）
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

/// 默认 Relay TLS 端口
pub const DEFAULT_RELAY_PORT: u16 = 8443;

/// 中继配置 (mode = "relay" 时使用)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayConfig {
    /// Aginx ID (首次启动自动从 API 申请)
    #[serde(default)]
    pub id: Option<String>,

    /// API 返回的 JWT token (首次申请后保存)
    #[serde(default)]
    pub token: Option<String>,

    /// Relay 域名 (用于构建连接地址和 URL)
    #[serde(default = "default_relay_domain")]
    pub domain: String,

    /// Relay 连接端口
    #[serde(default = "default_relay_port")]
    pub port: u16,

    /// 是否使用 TLS
    #[serde(default = "default_true")]
    pub use_tls: bool,

    /// Relay 完整地址 (优先于 domain+port 构建)
    #[serde(default)]
    pub url: Option<String>,

    /// 心跳间隔 (秒)
    #[serde(default = "default_heartbeat_interval")]
    pub heartbeat_interval: u64,

    /// 重连间隔 (秒)
    #[serde(default = "default_reconnect_interval")]
    pub reconnect_interval: u64,

    /// 是否向 aginx-api 上报 Agent 列表
    #[serde(default)]
    pub publish_agents: bool,
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
        }
    }
}

impl RelayConfig {
    /// 获取连接地址
    pub fn get_connect_url(&self) -> String {
        if let Some(ref url) = self.url {
            url.clone()
        } else if let Some(ref id) = self.id {
            format!("{}.{}:{}", id, self.domain, self.port)
        } else {
            format!("{}:{}", self.domain, self.port)
        }
    }

    /// 是否已配置 ID
    pub fn has_id(&self) -> bool {
        self.id.is_some()
    }

    /// 设置 ID（申请成功后调用）
    pub fn set_id(&mut self, id: String) {
        self.id = Some(id.clone());
        self.url = Some(format!("{}.{}:{}", id, self.domain, self.port));
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

/// API 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiConfig {
    /// API 服务器地址
    #[serde(default = "default_api_url")]
    pub url: String,
}

fn default_api_url() -> String { "https://api.yinnho.cn".to_string() }
fn default_agent_type() -> String { "process".to_string() }

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            url: default_api_url(),
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentsConfig {
    /// Agent 列表
    #[serde(default)]
    pub list: Vec<AgentEntry>,

    /// Agent 发现目录 (扫描 aginx.toml 的默认路径)
    /// 默认: ~/.aginx/agents/
    #[serde(default)]
    pub dir: Option<PathBuf>,
}

impl AgentsConfig {
    /// 获取 agents 目录
    pub fn get_agents_dir(&self) -> PathBuf {
        self.dir.clone().unwrap_or_else(|| {
            dirs::home_dir()
                .map(|h| h.join(".aginx").join("agents"))
                .unwrap_or_else(|| PathBuf::from(".aginx/agents"))
        })
    }
}

impl Default for AgentsConfig {
    fn default() -> Self {
        Self {
            list: vec![],
            dir: None,
        }
    }
}


/// Agent 配置项 (主配置文件中的条目)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEntry {
    /// Agent ID (唯一标识)
    pub id: String,
    /// Agent 名称
    pub name: String,
    /// Agent 类型
    #[serde(default = "default_agent_type")]
    pub agent_type: String,
    /// 能力标签
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    /// 描述
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,

    // Process 类型专用字段
    /// 进程命令 (type=process 时必填)
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub command: String,
    /// 进程参数
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// 工作目录 (固定目录)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<PathBuf>,
    /// 环境变量
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub env: std::collections::HashMap<String, String>,
    /// 需要移除的环境变量
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_remove: Vec<String>,
    /// Session ID 参数模板 (支持 ${SESSION_ID} 变量)
    /// 例如: ["--session-id", "${SESSION_ID}"]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub session_args: Vec<String>,

    /// 进程超时（秒），仅 process 类型。默认 60 秒
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,

    /// 通信协议: "acp" | "claude-stream"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,

    /// 会话配置
    #[serde(default)]
    pub session: SessionField,
}

/// 会话配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionField {
    /// 是否需要用户选择工作目录
    #[serde(default)]
    pub require_workdir: bool,
}

impl Default for SessionField {
    fn default() -> Self {
        Self {
            require_workdir: false,
        }
    }
}


