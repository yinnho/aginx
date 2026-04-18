//! Agent orchestrator for agent-to-agent communication.
//!
//! Allows agents to create sub-agents, send prompts, and coordinate tasks
//! through aginx as the routing layer.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;

use crate::agent::{AgentManager, SessionManager};

/// A sub-agent session created by a parent agent.
struct SubAgentSession {
    /// The sub-agent's aginx session ID
    sub_session_id: String,
    /// The target agent ID
    agent_id: String,
    /// The parent's aginx session ID
    parent_session_id: String,
    /// When this sub-agent was created
    created_at: Instant,
}

/// Manages sub-agent sessions for agent-to-agent communication.
pub struct AgentOrchestrator {
    /// Reference to the agent manager for resolving target agents
    agent_manager: Arc<AgentManager>,
    /// Reference to the session manager
    session_manager: Arc<SessionManager>,
    /// Active sub-agent sessions: sub_session_id → SubAgentSession
    sub_agents: Arc<Mutex<HashMap<String, SubAgentSession>>>,
}

impl AgentOrchestrator {
    pub fn new(
        agent_manager: Arc<AgentManager>,
        session_manager: Arc<SessionManager>,
    ) -> Self {
        Self {
            agent_manager,
            session_manager,
            sub_agents: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// List all available agents that can be used as sub-agents.
    pub async fn list_available_agents(&self) -> Vec<serde_json::Value> {
        self.agent_manager.list_agents().await
    }

    /// Create a sub-agent session.
    /// Returns the sub-agent's session ID on success.
    pub async fn create_sub_agent(
        &self,
        parent_session_id: &str,
        agent_id: &str,
        workdir: Option<&str>,
    ) -> Result<String, String> {
        // Verify the target agent exists
        let agent_info = self.agent_manager.get_agent_info(agent_id).await
            .ok_or_else(|| format!("Agent not found: {}", agent_id))?;

        // Create a new session for the sub-agent
        let sub_session_id = self.session_manager.create_session(&agent_info, workdir).await?;

        // Track the sub-agent
        self.sub_agents.lock().await.insert(sub_session_id.clone(), SubAgentSession {
            sub_session_id: sub_session_id.clone(),
            agent_id: agent_id.to_string(),
            parent_session_id: parent_session_id.to_string(),
            created_at: Instant::now(),
        });

        tracing::info!("Sub-agent created: parent={}, agent={}, sub_session={}",
            parent_session_id, agent_id, sub_session_id);

        Ok(sub_session_id)
    }

    /// Cancel a running sub-agent.
    pub async fn cancel_sub_agent(&self, sub_session_id: &str) -> Result<(), String> {
        let sub = self.sub_agents.lock().await.remove(sub_session_id)
            .ok_or_else(|| format!("Sub-agent session not found: {}", sub_session_id))?;

        self.session_manager.close_session(sub_session_id).await
            .map_err(|e| format!("Failed to close sub-agent session: {}", e))?;

        tracing::info!("Sub-agent canceled: sub_session={}, agent={}",
            sub_session_id, sub.agent_id);
        Ok(())
    }

    /// List all sub-agents for a given parent session.
    pub async fn list_sub_agents(&self, parent_session_id: &str) -> Vec<serde_json::Value> {
        let subs = self.sub_agents.lock().await;
        subs.values()
            .filter(|s| s.parent_session_id == parent_session_id)
            .map(|s| serde_json::json!({
                "subSessionId": s.sub_session_id,
                "agentId": s.agent_id,
                "createdAt": s.created_at.elapsed().as_secs(),
            }))
            .collect()
    }
}
