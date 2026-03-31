//! Agent Manager for aginx
//!
//! 从配置文件加载 agent，支持 builtin、claude、process 三种类型

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::config::{AgentConfig, AgentType, Config};

/// Agent Manager
#[derive(Clone)]
pub struct AgentManager {
    agents: Arc<RwLock<HashMap<String, AgentInfo>>>,
}

/// Agent information (运行时信息)
#[derive(Clone, Debug)]
pub struct AgentInfo {
    pub id: String,
    pub name: String,
    pub capabilities: Vec<String>,
    pub agent_type: AgentType,
    pub command: String,
    pub args: Vec<String>,
    pub help_command: String,
    pub working_dir: Option<String>,
    pub require_workdir: bool,
    pub env: HashMap<String, String>,
    /// 需要移除的环境变量
    pub env_remove: Vec<String>,
    /// Session ID 参数模板 (支持 ${SESSION_ID} 变量)
    pub session_args: Vec<String>,
    /// 默认允许的工具列表 (用于 --permission-mode dontAsk --allowedTools)
    pub default_allowed_tools: Vec<String>,
}

impl From<AgentConfig> for AgentInfo {
    fn from(config: AgentConfig) -> Self {
        Self {
            id: config.id,
            name: config.name,
            capabilities: config.capabilities,
            agent_type: config.agent_type,
            command: config.command,
            args: config.args,
            help_command: config.help_command,
            working_dir: config.working_dir.map(|p| p.to_string_lossy().to_string()),
            require_workdir: config.session.require_workdir,
            env: config.env,
            env_remove: config.env_remove,
            session_args: config.session_args,
            default_allowed_tools: Vec::new(),
        }
    }
}

impl AgentManager {
    /// Create agent manager from config
    pub fn from_config(config: &Config) -> Self {
        let mut agents: HashMap<String, AgentInfo> = config
            .agents
            .list
            .iter()
            .map(|ac| (ac.id.clone(), AgentInfo::from(ac.clone())))
            .collect();

        // 添加内置 agents（总是可用）
        let builtin_agents = vec![
            AgentInfo {
                id: "echo".to_string(),
                name: "Echo".to_string(),
                capabilities: vec!["chat".to_string()],
                agent_type: AgentType::Builtin,
                command: String::new(),
                args: vec![],
                help_command: String::new(),
                working_dir: None,
                require_workdir: false,
                env: HashMap::new(),
                env_remove: vec![],
                session_args: vec![],
                default_allowed_tools: vec![],
            },
            AgentInfo {
                id: "info".to_string(),
                name: "Info".to_string(),
                capabilities: vec!["chat".to_string()],
                agent_type: AgentType::Builtin,
                command: String::new(),
                args: vec![],
                help_command: String::new(),
                working_dir: None,
                require_workdir: false,
                env: HashMap::new(),
                env_remove: vec![],
                session_args: vec![],
                default_allowed_tools: vec![],
            },
            // Shell agent - 使用 process 类型，每次执行一个命令
            AgentInfo {
                id: "shell".to_string(),
                name: "Shell".to_string(),
                capabilities: vec!["chat".to_string(), "code".to_string()],
                agent_type: AgentType::Process,
                command: "sh".to_string(),
                args: vec!["-c".to_string()],
                help_command: String::new(),
                working_dir: None,
                require_workdir: false,
                env: HashMap::new(),
                env_remove: vec![],
                session_args: vec![],
                default_allowed_tools: vec![],
            },
        ];

        for agent in builtin_agents {
            if !agents.contains_key(&agent.id) {
                agents.insert(agent.id.clone(), agent);
            }
        }

        // 自动扫描 ~/.aginx/agents/ 目录，加载 aginx.toml 配置的 agent
        let agents_dir = dirs::home_dir()
            .map(|h| h.join(".aginx").join("agents"))
            .unwrap_or_else(|| std::path::PathBuf::from(".aginx/agents"));

        if agents_dir.exists() {
            let discovered = super::discovery::scan_directory(&agents_dir, 5);
            for agent in discovered {
                if agent.available {
                    let dc = &agent.config;
                    if agents.contains_key(&dc.id) {
                        continue;
                    }

                    // 将 discovery::AgentConfig 转换为 AgentInfo
                    let agent_type = match dc.agent_type.as_str() {
                        "claude" => AgentType::Claude,
                        "process" => AgentType::Process,
                        _ => AgentType::Builtin,
                    };

                    let command = dc.command.as_ref()
                        .and_then(|c| c.path.clone())
                        .unwrap_or_default();
                    let args = dc.command.as_ref()
                        .map(|c| c.args.clone())
                        .unwrap_or_default();
                    let env = dc.command.as_ref()
                        .map(|c| c.env.clone())
                        .unwrap_or_default();
                    let env_remove = dc.command.as_ref()
                        .map(|c| c.env_remove.clone())
                        .unwrap_or_default();
                    let session_args = dc.session.as_ref()
                        .and_then(|s| s.resume.as_ref())
                        .map(|r| r.resume_args.clone())
                        .unwrap_or_default();
                    let require_workdir = dc.session.as_ref()
                        .map(|s| s.require_workdir)
                        .unwrap_or(false);

                    let mut capabilities = Vec::new();
                    if let Some(ref caps) = dc.capabilities {
                        if caps.chat { capabilities.push("chat".to_string()); }
                        if caps.code { capabilities.push("code".to_string()); }
                        if caps.streaming { capabilities.push("streaming".to_string()); }
                    }

                    let default_allowed_tools = dc.permissions.as_ref()
                        .map(|p| p.default_allowed.clone())
                        .unwrap_or_default();

                    let info = AgentInfo {
                        id: dc.id.clone(),
                        name: dc.name.clone(),
                        capabilities,
                        agent_type,
                        command,
                        args,
                        help_command: String::new(),
                        working_dir: Some(agent.project_dir.to_string_lossy().to_string()),
                        require_workdir,
                        env,
                        env_remove,
                        session_args,
                        default_allowed_tools,
                    };

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
                    "agent_type": match a.agent_type {
                        AgentType::Claude => "claude",
                        AgentType::Process => "process",
                        AgentType::Builtin => "builtin",
                    },
                    "capabilities": a.capabilities,
                    "require_workdir": a.require_workdir,
                    "working_dir": a.working_dir
                })
            })
            .collect()
    }

    /// Get agent count
    pub async fn agent_count(&self) -> usize {
        let agents = self.agents.read().await;
        agents.len()
    }

    /// Get agent card
    pub async fn get_agent_card(&self, agent_id: &str) -> Option<(String, Vec<String>)> {
        let agents = self.agents.read().await;
        agents
            .get(agent_id)
            .map(|a| (a.name.clone(), a.capabilities.clone()))
    }

    /// Check if agent exists
    pub async fn has_agent(&self, agent_id: &str) -> bool {
        let agents = self.agents.read().await;
        agents.contains_key(agent_id)
    }

    /// Get agent info
    pub async fn get_agent_info(&self, agent_id: &str) -> Option<AgentInfo> {
        let agents = self.agents.read().await;
        agents.get(agent_id).cloned()
    }

    /// Get agent help (执行 help_command 获取帮助信息)
    pub async fn get_agent_help(&self, agent_id: &str) -> Result<String, String> {
        use tokio::process::Command;
        use tokio::time::{timeout, Duration};

        let agents = self.agents.read().await;
        let agent = agents
            .get(agent_id)
            .ok_or_else(|| format!("Agent not found: {}", agent_id))?;

        // 确定要执行的 help 命令
        let help_cmd = if !agent.help_command.is_empty() {
            agent.help_command.clone()
        } else if !agent.command.is_empty() {
            // fallback: command --help
            format!("{} --help", agent.command)
        } else {
            return Ok(format!(
                "Agent: {}\nType: {:?}\nCapabilities: {}",
                agent.name,
                agent.agent_type,
                agent.capabilities.join(", ")
            ));
        };

        tracing::info!("获取 Agent [{}] 帮助: {}", agent_id, help_cmd);

        // 解析命令
        let parts: Vec<&str> = help_cmd.split_whitespace().collect();
        if parts.is_empty() {
            return Err("Invalid help_command".to_string());
        }

        let mut cmd = Command::new(parts[0]);
        for part in &parts[1..] {
            cmd.arg(*part);
        }

        // 设置工作目录和环境变量
        if let Some(ref dir) = agent.working_dir {
            cmd.current_dir(dir);
        }
        for (key, value) in &agent.env {
            cmd.env(key, value);
        }

        // 执行命令，带超时保护
        let result = timeout(Duration::from_secs(10), cmd.output()).await;

        match result {
            Ok(Ok(output)) => {
                if output.status.success() {
                    let help = String::from_utf8_lossy(&output.stdout).to_string();
                    Ok(help)
                } else {
                    let error = String::from_utf8_lossy(&output.stderr).to_string();
                    // 有些命令把 help 输出到 stderr
                    if !error.is_empty() {
                        Ok(error)
                    } else {
                        Err("No help output".to_string())
                    }
                }
            }
            Ok(Err(e)) => Err(format!("Failed to execute help command: {}", e)),
            Err(_) => Err("Help command timeout (>10s)".to_string()),
        }
    }

    /// Register a new agent (runtime)
    pub async fn register_agent(&self, info: AgentInfo) {
        let mut agents = self.agents.write().await;
        tracing::info!("注册 agent: {} ({})", info.id, info.name);
        agents.insert(info.id.clone(), info);
    }
}
