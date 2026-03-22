//! Message types for aginx

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{JsonRpcRequest, JsonRpcResponse, JsonRpcErrorObject, RequestId};

/// JSON-RPC method names
pub mod methods {
    pub const SEND_MESSAGE: &str = "sendMessage";
    pub const GET_AGENT_CARD: &str = "getAgentCard";
    pub const LIST_AGENTS: &str = "listAgents";
    pub const GET_SERVER_INFO: &str = "getServerInfo";
}

/// Protocol errors
#[derive(Error, Debug)]
pub enum ProtocolError {
    #[error("Invalid message format: {0}")]
    InvalidFormat(String),

    #[error("Parse error: {0}")]
    ParseError(String),

    #[error("JSON-RPC error: {0}")]
    JsonRpcError(String),
}

/// Message types for aginx
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Message {
    /// Send message to agent
    Send {
        agent: String,
        message: String,
    },

    /// Send response
    SendOk {
        response: String,
    },

    /// Error response
    Error {
        code: u16,
        message: String,
    },

    /// Server info
    ServerInfo {
        version: String,
        relay_url: Option<String>,
    },

    /// List agents
    ListAgents,

    /// List agents response
    ListAgentsOk {
        agents: Vec<String>,
    },

    /// Get agent card
    GetAgentCard {
        agent_id: String,
    },

    /// Agent card response
    AgentCardOk {
        id: String,
        name: String,
        capabilities: Vec<String>,
    },
}

impl Message {
    /// Get the message type name
    pub fn type_name(&self) -> &'static str {
        match self {
            Message::Send { .. } => "SEND",
            Message::SendOk { .. } => "SEND_OK",
            Message::Error { .. } => "ERROR",
            Message::ServerInfo { .. } => "SERVER_INFO",
            Message::ListAgents => "LIST_AGENTS",
            Message::ListAgentsOk { .. } => "LIST_AGENTS_OK",
            Message::GetAgentCard { .. } => "GET_AGENT_CARD",
            Message::AgentCardOk { .. } => "AGENT_CARD_OK",
        }
    }

    /// Check if this is an error response
    pub fn is_error(&self) -> bool {
        matches!(self, Message::Error { .. })
    }

    /// Convert to JSON-RPC request
    pub fn to_jsonrpc_request(&self, id: impl Into<RequestId>) -> JsonRpcRequest {
        let id = id.into();
        match self {
            Message::Send { agent, message } => {
                JsonRpcRequest::new(id, methods::SEND_MESSAGE, Some(serde_json::json!({
                    "agentId": agent,
                    "message": message
                })))
            }
            Message::ListAgents => {
                JsonRpcRequest::new(id, methods::LIST_AGENTS, None)
            }
            Message::GetAgentCard { agent_id } => {
                JsonRpcRequest::new(id, methods::GET_AGENT_CARD, Some(serde_json::json!({
                    "agentId": agent_id
                })))
            }
            _ => JsonRpcRequest::new(id, "unknown", None),
        }
    }

    /// Convert from JSON-RPC response
    pub fn from_jsonrpc_response(response: &JsonRpcResponse) -> Result<Self, ProtocolError> {
        if let Some(error) = &response.error {
            return Ok(Message::Error {
                code: error.code as u16,
                message: error.message.clone(),
            });
        }

        let result = response.result.as_ref().ok_or_else(|| {
            ProtocolError::InvalidFormat("Response has no result".to_string())
        })?;

        // SendOk
        if let Some(response_text) = result.get("response") {
            return Ok(Message::SendOk {
                response: response_text.as_str().unwrap_or_default().to_string(),
            });
        }

        // ListAgentsOk
        if let Some(agents) = result.get("agents") {
            let agent_list = agents.as_array()
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default();
            return Ok(Message::ListAgentsOk { agents: agent_list });
        }

        // AgentCardOk
        if let Some(id) = result.get("id") {
            let name = result.get("name")
                .and_then(|n| n.as_str())
                .unwrap_or_default()
                .to_string();
            let capabilities = result.get("capabilities")
                .and_then(|c| c.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default();
            return Ok(Message::AgentCardOk {
                id: id.as_str().unwrap_or_default().to_string(),
                name,
                capabilities,
            });
        }

        // ServerInfo
        if let Some(version) = result.get("version") {
            let relay_url = result.get("relayUrl")
                .and_then(|u| u.as_str())
                .map(String::from);
            return Ok(Message::ServerInfo {
                version: version.as_str().unwrap_or_default().to_string(),
                relay_url,
            });
        }

        // Default: treat as SendOk
        Ok(Message::SendOk {
            response: result.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_type_name() {
        let msg = Message::Send {
            agent: "hotel".to_string(),
            message: "hello".to_string(),
        };
        assert_eq!(msg.type_name(), "SEND");
    }

    #[test]
    fn test_message_to_jsonrpc() {
        let msg = Message::Send {
            agent: "hotel".to_string(),
            message: "查询房间".to_string(),
        };
        let req = msg.to_jsonrpc_request(1);
        assert_eq!(req.method, "sendMessage");
    }
}
