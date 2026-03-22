//! Agent Manager for aginx

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

/// Agent Manager
#[derive(Clone)]
pub struct AgentManager {
    agents: Arc<RwLock<HashMap<String, AgentInfo>>>,
}

/// Agent information
#[derive(Clone, Debug)]
pub struct AgentInfo {
    pub id: String,
    pub name: String,
    pub capabilities: Vec<String>,
    pub agent_type: AgentType,
}

/// Agent type
#[derive(Clone, Debug)]
pub enum AgentType {
    Builtin,
    Process(ProcessConfig),
}

/// Process agent configuration
#[derive(Clone, Debug)]
pub struct ProcessConfig {
    pub command: String,
    pub args: Vec<String>,
    pub working_dir: Option<String>,
    pub env: HashMap<String, String>,
}

impl AgentManager {
    /// Create a new agent manager
    pub fn new() -> Self {
        let mut agents = HashMap::new();

        // Register built-in agents
        agents.insert("echo".to_string(), AgentInfo {
            id: "echo".to_string(),
            name: "Echo Agent".to_string(),
            capabilities: vec!["echo".to_string()],
            agent_type: AgentType::Builtin,
        });

        agents.insert("info".to_string(), AgentInfo {
            id: "info".to_string(),
            name: "Info Agent".to_string(),
            capabilities: vec!["info".to_string()],
            agent_type: AgentType::Builtin,
        });

        Self {
            agents: Arc::new(RwLock::new(agents)),
        }
    }

    /// List all agents
    pub async fn list_agents(&self) -> Vec<serde_json::Value> {
        let agents = self.agents.read().await;
        agents.values().map(|a| {
            serde_json::json!({
                "id": a.id,
                "name": a.name,
                "capabilities": a.capabilities
            })
        }).collect()
    }

    /// Get agent count
    pub async fn agent_count(&self) -> usize {
        let agents = self.agents.read().await;
        agents.len()
    }

    /// Get agent card
    pub async fn get_agent_card(&self, agent_id: &str) -> Option<(String, Vec<String>)> {
        let agents = self.agents.read().await;
        agents.get(agent_id).map(|a| (a.name.clone(), a.capabilities.clone()))
    }

    /// Send message to agent
    pub async fn send_message(&self, agent_id: &str, message: &str) -> Result<String, String> {
        let agents = self.agents.read().await;
        let agent = agents.get(agent_id)
            .ok_or_else(|| format!("Agent not found: {}", agent_id))?;

        match &agent.agent_type {
            AgentType::Builtin => {
                // Built-in agents
                match agent_id {
                    "echo" => Ok(message.to_string()),
                    "info" => Ok(format!("aginx v0.1.0 - Agent: {}", agent.name)),
                    _ => Ok(format!("[{}] {}", agent_id, message)),
                }
            }
            AgentType::Process(config) => {
                // Process agents - TODO: implement
                Err(format!("Process agent not implemented yet: {}", config.command))
            }
        }
    }

    /// Register a process agent
    pub async fn register_agent(&self, info: AgentInfo) {
        let mut agents = self.agents.write().await;
        agents.insert(info.id.clone(), info);
    }
}

impl Default for AgentManager {
    fn default() -> Self {
        Self::new()
    }
}
