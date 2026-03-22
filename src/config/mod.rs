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
    Direct,
    /// 中继模式 - 连接 relay 服务器（默认）
    #[default]
    Relay,
}

/// 默认 Relay 服务器地址
pub const DEFAULT_RELAY_SERVER: &str = "relay.yinnho.cn:8600";

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
    /// Aginx ID (首次启动自动申请)
    #[serde(default)]
    pub id: Option<String>,

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
            id: None,
            url: None,
            heartbeat_interval: default_heartbeat_interval(),
            reconnect_interval: default_reconnect_interval(),
        }
    }
}

impl RelayConfig {
    /// 获取连接地址
    /// 如果有 url 则使用 url，否则用默认服务器地址
    pub fn get_connect_url(&self) -> String {
        if let Some(ref url) = self.url {
            url.clone()
        } else if let Some(ref id) = self.id {
            format!("{}.relay.yinnho.cn:8600", id)
        } else {
            DEFAULT_RELAY_SERVER.to_string()
        }
    }

    /// 是否已配置 ID
    pub fn has_id(&self) -> bool {
        self.id.is_some()
    }

    /// 设置 ID（申请成功后调用）
    pub fn set_id(&mut self, id: String) {
        self.id = Some(id.clone());
        self.url = Some(format!("{}.relay.yinnho.cn:8600", id));
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentsConfig {
    /// Agent 列表
    #[serde(default)]
    pub list: Vec<AgentConfig>,
}

/// Agent 类型
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum AgentType {
    /// 内置 agent (echo, info 等)
    Builtin,
    /// Claude CLI
    Claude,
    /// 外部进程
    Process,
}

impl Default for AgentType {
    fn default() -> Self {
        Self::Builtin
    }
}

/// Agent 配置 (统一格式)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Agent ID (唯一标识)
    pub id: String,
    /// Agent 名称
    pub name: String,
    /// Agent 类型
    #[serde(default)]
    pub agent_type: AgentType,
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
    /// 工作目录
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<PathBuf>,
    /// 环境变量
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub env: std::collections::HashMap<String, String>,
}

impl AgentConfig {
    /// 创建内置 agent 配置
    pub fn builtin(id: &str, name: &str, capabilities: Vec<&str>) -> Self {
        Self {
            id: id.to_string(),
            name: name.to_string(),
            agent_type: AgentType::Builtin,
            capabilities: capabilities.iter().map(|s| s.to_string()).collect(),
            description: String::new(),
            command: String::new(),
            args: Vec::new(),
            working_dir: None,
            env: std::collections::HashMap::new(),
        }
    }

    /// 创建 Claude agent 配置
    pub fn claude() -> Self {
        Self {
            id: "claude".to_string(),
            name: "Claude Agent".to_string(),
            agent_type: AgentType::Claude,
            capabilities: vec!["chat".to_string(), "code".to_string(), "ask".to_string()],
            description: "AI programming assistant".to_string(),
            command: String::new(),
            args: Vec::new(),
            working_dir: None,
            env: std::collections::HashMap::new(),
        }
    }
}

impl Default for AgentsConfig {
    fn default() -> Self {
        Self {
            list: vec![
                AgentConfig::builtin("echo", "Echo Agent", vec!["echo"]),
                AgentConfig::builtin("info", "Info Agent", vec!["info"]),
                AgentConfig::claude(),
            ],
        }
    }
}
