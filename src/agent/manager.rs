//! Agent Manager for aginx
//!
//! Loads agents from config + ~/.aginx/agents/ directory

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::config::{AccessMode, Config};

/// Agent Manager
#[derive(Clone)]
pub struct AgentManager {
    agents: Arc<RwLock<HashMap<String, AgentInfo>>>,
    /// 会话台账（sessions/list 事实源，§2.4.1）
    pub ledger: super::ledger::SessionLedger,
}

/// Agent runtime info
#[derive(Clone, Debug)]
pub struct AgentInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    pub agent_type: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub timeout: Option<u64>,
    /// Resume args template (e.g. ["--resume", "${SESSION_ID}"])
    pub resume_args: Option<Vec<String>>,
    /// stdout dialect declaration (ACP.md §2.8), e.g. "claude-stream-json"
    pub output: Option<String>,
    pub working_dir: Option<String>,
    pub access: AccessMode,
}

impl AgentManager {
    /// Create agent manager from config
    pub fn from_config(config: &Config) -> Self {
        let global_access = config.server.access;
        let mut agents: HashMap<String, AgentInfo> = HashMap::new();

        // Load from config.agents.list
        for ac in &config.agents.list {
            let info = AgentInfo {
                id: ac.id.clone(),
                name: ac.name.clone(),
                description: ac.description.clone(),
                agent_type: ac.agent_type.clone(),
                command: ac.command.clone(),
                args: ac.args.clone(),
                env: ac.env.clone(),
                timeout: ac.timeout,
                resume_args: None,
                output: ac.output.clone(),
                working_dir: ac.working_dir.as_ref().map(|p| p.to_string_lossy().to_string()),
                access: global_access,
            };
            agents.insert(ac.id.clone(), info);
        }

        // Auto-scan ~/.aginx/agents/
        let agents_dir = crate::config::agents_dir();
        if agents_dir.exists() {
            let discovered = super::discovery::scan_directory(&agents_dir, 5);
            // 注册项 type 继承：无 [command] 且 agent_type 命中另一注册项 id
            // → 继承其 command/session/output/timeout（type=模板，注册项=实例）
            let type_sources: HashMap<String, super::discovery::AgentConfig> = discovered
                .iter()
                .filter(|a| a.available && a.config.command.is_some())
                .map(|a| (a.config.agent_type.clone(), a.config.clone()))
                .collect();
            for agent in discovered {
                if agent.available {
                    if agents.contains_key(&agent.config.id) {
                        continue;
                    }
                    let mut config = agent.config;
                    if config.command.is_none() {
                        if let Some(src) = type_sources.get(&config.agent_type) {
                            config.command = src.command.clone();
                            config.session = src.session.clone();
                            config.output = config.output.clone().or_else(|| src.output.clone());
                            config.timeout = config.timeout.or(src.timeout);
                            tracing::info!(
                                "Agent {} inherits CLI details from type '{}'",
                                config.id,
                                config.agent_type
                            );
                        }
                    }
                    let info = super::discovery::agent_config_to_info(
                        config,
                        &agent.project_dir,
                        &global_access,
                    );
                    tracing::info!("Auto-loaded agent: {} ({})", info.id, info.name);
                    agents.insert(info.id.clone(), info);
                } else {
                    tracing::warn!("Skipping unavailable agent {}: {:?}", agent.config.id, agent.error);
                }
            }
        }

        tracing::info!("Loaded {} agents", agents.len());
        Self {
            agents: Arc::new(RwLock::new(agents)),
            ledger: super::ledger::SessionLedger::load(),
        }
    }

    /// List all agents as JSON
    pub async fn list_agents(&self) -> Vec<serde_json::Value> {
        let agents = self.agents.read().await;
        agents.values().map(|a| {
            serde_json::json!({
                "id": a.id,
                "name": a.name,
                "description": a.description,
                "agent_type": a.agent_type,
            })
        }).collect()
    }

    /// Get agent info by ID
    pub async fn get_agent_info(&self, agent_id: &str) -> Option<AgentInfo> {
        let agents = self.agents.read().await;
        agents.get(agent_id).cloned()
    }

    /// Register a new agent at runtime
    pub async fn register_agent(&self, info: AgentInfo) {
        let mut agents = self.agents.write().await;
        tracing::info!("Registered agent: {} ({})", info.id, info.name);
        agents.insert(info.id.clone(), info);
    }

    /// Check if any agent is loaded
    pub fn has_agents(&self) -> bool {
        match self.agents.try_read() {
            Ok(agents) => !agents.is_empty(),
            Err(_) => false,
        }
    }
}
