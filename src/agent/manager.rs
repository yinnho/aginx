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
            require_workdir: config.require_workdir,
            env: config.env,
            env_remove: config.env_remove,
            session_args: config.session_args,
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

    /// Send message to agent
    pub async fn send_message(&self, agent_id: &str, message: &str, workdir: Option<&str>) -> Result<String, String> {
        let agents = self.agents.read().await;
        let agent = agents
            .get(agent_id)
            .ok_or_else(|| format!("Agent not found: {}", agent_id))?;

        // 确定工作目录：优先使用传入的，其次使用配置的
        let effective_workdir = workdir
            .map(|s| s.to_string())
            .or_else(|| agent.working_dir.clone());

        match &agent.agent_type {
            AgentType::Builtin => self.handle_builtin(agent_id, message).await,
            AgentType::Claude => self.call_claude(agent, message, effective_workdir.as_deref()).await,
            AgentType::Process => self.call_process_with_dir(agent, message, effective_workdir.as_deref()).await,
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
    async fn call_claude(&self, agent: &AgentInfo, message: &str, workdir: Option<&str>) -> Result<String, String> {
        use tokio::process::Command;

        tracing::info!("调用 Claude CLI: {} (workdir: {:?})", message, workdir);

        // 使用配置的 command 或默认 "claude"
        let claude_cmd = if agent.command.is_empty() {
            "claude"
        } else {
            &agent.command
        };

        let mut cmd = Command::new(claude_cmd);
        cmd.arg("--print").arg(message);

        // 使用配置的 env_remove
        for env in &agent.env_remove {
            cmd.env_remove(env);
        }

        // 设置工作目录
        if let Some(dir) = workdir {
            cmd.current_dir(dir);
        }

        let output = cmd
            .output()
            .await
            .map_err(|e| format!("Failed to spawn {}: {}", claude_cmd, e))?;

        if output.status.success() {
            let result = String::from_utf8_lossy(&output.stdout).to_string();
            tracing::info!("Claude 响应: {} bytes", result.len());
            Ok(result)
        } else {
            let error = String::from_utf8_lossy(&output.stderr).to_string();
            Err(format!("Claude CLI error: {}", error))
        }
    }

    /// 调用外部进程（带工作目录参数）
    async fn call_process_with_dir(&self, agent: &AgentInfo, message: &str, workdir: Option<&str>) -> Result<String, String> {
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

        // 设置工作目录（优先使用传入的，其次使用配置的）
        if let Some(dir) = workdir {
            cmd.current_dir(dir);
        } else if let Some(ref dir) = agent.working_dir {
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
