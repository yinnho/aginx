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
    pub working_dir: Option<String>,
    pub env: HashMap<String, String>,
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
            working_dir: config.working_dir.map(|p| p.to_string_lossy().to_string()),
            env: config.env,
        }
    }
}

impl AgentManager {
    /// Create agent manager from config
    pub fn from_config(config: &Config) -> Self {
        let agents: HashMap<String, AgentInfo> = config
            .agents
            .list
            .iter()
            .map(|ac| (ac.id.clone(), AgentInfo::from(ac.clone())))
            .collect();

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
                    "capabilities": a.capabilities
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

    /// Send message to agent
    pub async fn send_message(&self, agent_id: &str, message: &str) -> Result<String, String> {
        let agents = self.agents.read().await;
        let agent = agents
            .get(agent_id)
            .ok_or_else(|| format!("Agent not found: {}", agent_id))?;

        match &agent.agent_type {
            AgentType::Builtin => self.handle_builtin(agent_id, message).await,
            AgentType::Claude => self.call_claude(message).await,
            AgentType::Process => self.call_process(agent, message).await,
        }
    }

    /// Handle builtin agent
    async fn handle_builtin(&self, agent_id: &str, message: &str) -> Result<String, String> {
        match agent_id {
            "echo" => Ok(message.to_string()),
            "info" => Ok("aginx v0.1.0 - Agent Protocol Implementation".to_string()),
            _ => Ok(format!("[{}] {}", agent_id, message)),
        }
    }

    /// 调用 Claude CLI
    async fn call_claude(&self, message: &str) -> Result<String, String> {
        use tokio::process::Command;

        tracing::info!("调用 Claude CLI: {}", message);

        let output = Command::new("claude")
            .arg("--print")
            .arg(message)
            .env_remove("CLAUDECODE")
            .output()
            .await
            .map_err(|e| format!("Failed to execute claude: {}", e))?;

        if output.status.success() {
            let result = String::from_utf8_lossy(&output.stdout).to_string();
            tracing::info!("Claude 响应: {} bytes", result.len());
            Ok(result)
        } else {
            let error = String::from_utf8_lossy(&output.stderr).to_string();
            Err(format!("Claude CLI error: {}", error))
        }
    }

    /// 调用外部进程
    async fn call_process(&self, agent: &AgentInfo, message: &str) -> Result<String, String> {
        use tokio::process::Command;

        if agent.command.is_empty() {
            return Err(format!("Agent {} has no command configured", agent.id));
        }

        tracing::info!("调用进程 [{}]: {}", agent.id, agent.command);

        let mut cmd = Command::new(&agent.command);

        // 添加参数
        for arg in &agent.args {
            cmd.arg(arg);
        }

        // 添加消息作为最后一个参数或通过 stdin
        cmd.arg(message);

        // 设置工作目录
        if let Some(ref dir) = agent.working_dir {
            cmd.current_dir(dir);
        }

        // 设置环境变量
        for (key, value) in &agent.env {
            cmd.env(key, value);
        }

        let output = cmd
            .output()
            .await
            .map_err(|e| format!("Failed to execute {}: {}", agent.command, e))?;

        if output.status.success() {
            let result = String::from_utf8_lossy(&output.stdout).to_string();
            tracing::info!("进程 [{}] 响应: {} bytes", agent.id, result.len());
            Ok(result)
        } else {
            let error = String::from_utf8_lossy(&output.stderr).to_string();
            Err(format!("Process error: {}", error))
        }
    }

    /// Register a new agent (runtime)
    pub async fn register_agent(&self, info: AgentInfo) {
        let mut agents = self.agents.write().await;
        tracing::info!("注册 agent: {} ({})", info.id, info.name);
        agents.insert(info.id.clone(), info);
    }
}
