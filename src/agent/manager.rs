//! Agent Manager for aginx
//!
//! 从配置文件和 ~/.aginx/agents/ 目录加载 agent，支持 claude、process 两种类型

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::config::{AccessMode, AgentEntry, Config};

/// Agent Manager
#[derive(Clone)]
pub struct AgentManager {
    agents: Arc<RwLock<HashMap<String, AgentInfo>>>,
}

/// Agent information (运行时信息)
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct AgentInfo {
    pub id: String,
    pub name: String,
    pub capabilities: Vec<String>,
    pub agent_type: String,
    pub command: String,
    pub args: Vec<String>,
    pub working_dir: Option<String>,
    pub require_workdir: bool,
    pub env: HashMap<String, String>,
    /// 需要移除的环境变量
    pub env_remove: Vec<String>,
    /// Session ID 参数模板 (支持 ${SESSION_ID} 变量)
    pub session_args: Vec<String>,
    /// 默认允许的工具列表 (用于 --permission-mode dontAsk --allowedTools)
    pub default_allowed_tools: Vec<String>,
    /// 访问模式: public | private
    pub access: AccessMode,
    /// 会话存储路径（Agent 自己的会话文件目录）
    pub storage_path: Option<String>,
    /// 通信协议: "acp" | "claude-stream"
    pub protocol: String,
    /// 进程超时（秒），process 类型专用
    pub timeout: Option<u64>,
}

impl From<AgentEntry> for AgentInfo {
    fn from(config: AgentEntry) -> Self {
        Self {
            id: config.id,
            name: config.name,
            capabilities: config.capabilities,
            agent_type: config.agent_type,
            command: config.command,
            args: config.args,
            working_dir: config.working_dir.map(|p| p.to_string_lossy().to_string()),
            require_workdir: config.session.require_workdir,
            env: config.env,
            env_remove: config.env_remove,
            session_args: config.session_args,
            default_allowed_tools: Vec::new(),
            access: AccessMode::default(),
            storage_path: None,
            protocol: "acp".to_string(),
            timeout: config.timeout,
        }
    }
}

impl AgentManager {
    /// Create agent manager from config
    pub fn from_config(config: &Config) -> Self {
        let global_access = config.server.access;
        let mut agents: HashMap<String, AgentInfo> = config
            .agents
            .list
            .iter()
            .map(|ac| {
                let mut info = AgentInfo::from(ac.clone());
                info.access = global_access;
                (ac.id.clone(), info)
            })
            .collect();

        // 自动扫描 ~/.aginx/agents/ 目录，加载 aginx.toml 配置的 agent
        let agents_dir = crate::config::agents_dir();

        if agents_dir.exists() {
            let discovered = super::discovery::scan_directory(&agents_dir, 5);
            for agent in discovered {
                if agent.available {
                    if agents.contains_key(&agent.config.id) {
                        continue;
                    }

                    let info = super::discovery::agent_config_to_info(
                        agent.config,
                        &agent.project_dir,
                        &global_access,
                    );

                    tracing::info!("自动加载 agent: {} ({})", info.id, info.name);
                    agents.insert(info.id.clone(), info);
                } else {
                    tracing::warn!("跳过不可用的 agent {}: {:?}", agent.config.id, agent.error);
                }
            }
        }

        tracing::info!("已加载 {} 个 agent", agents.len());
        for (id, info) in &agents {
            tracing::debug!("  - {} ({:?}): {}", id, info.agent_type, info.name);
        }

        Self {
            agents: Arc::new(RwLock::new(agents)),
        }
    }

    /// List all agents
    pub async fn list_agents(&self) -> Vec<serde_json::Value> {
        let agents = self.agents.read().await;
        agents
            .values()
            .map(|a| {
                serde_json::json!({
                    "id": a.id,
                    "name": a.name,
                    "agent_type": a.agent_type,
                    "capabilities": a.capabilities,
                    "require_workdir": a.require_workdir,
                    "working_dir": a.working_dir,
                    "access": match a.access {
                        AccessMode::Public => "public",
                        AccessMode::Private => "private",
                    }
                })
            })
            .collect()
    }

    /// Get agent info
    pub async fn get_agent_info(&self, agent_id: &str) -> Option<AgentInfo> {
        let agents = self.agents.read().await;
        agents.get(agent_id).cloned()
    }

    /// Register a new agent (runtime)
    pub async fn register_agent(&self, info: AgentInfo) {
        let mut agents = self.agents.write().await;
        tracing::info!("注册 agent: {} ({})", info.id, info.name);
        agents.insert(info.id.clone(), info);
    }

    /// Check if any agent is loaded (sync, for startup validation)
    pub fn has_agents(&self) -> bool {
        // Use try_read to avoid blocking — we just created it so no write locks exist
        match self.agents.try_read() {
            Ok(agents) => !agents.is_empty(),
            Err(_) => true, // if locked, assume agents exist
        }
    }
}
