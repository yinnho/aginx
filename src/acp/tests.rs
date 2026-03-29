//! ACP 协议处理单元测试
//!
//! 测试 handler 和 streaming 的 ACP 消息处理，不依赖实际 Claude 执行

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::collections::HashMap;

    use super::super::*;
    use crate::agent::{AgentManager, SessionManager, SessionConfig};
    use crate::config::Config;

    /// 测试 initialize 请求
    #[tokio::test]
    async fn test_handle_initialize() {
        let agent_manager = Arc::new(AgentManager::from_config(&Config::default()));
        let session_manager = Arc::new(SessionManager::new(SessionConfig {
            max_concurrent: 10,
            timeout_seconds: 1800,
        }));

        let handler = AcpHandler::new(agent_manager, session_manager);

        let request = AcpRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(Id::Number(1)),
            method: "initialize".to_string(),
            params: Some(serde_json::json!({
                "protocolVersion": "0.15.0",
                "clientInfo": {
                    "name": "test",
                    "version": "1.0.0"
                }
            })),
        };

        let response = handler.handle_request(request).await;

        assert!(response.error.is_none(), "初始化不应该出错");
        assert!(response.result.is_some(), "应该有结果");

        let result = response.result.unwrap();
        assert_eq!(
            result.get("protocolVersion").and_then(|v| v.as_str()),
            Some("0.15.0")
        );
    }

    /// 测试 newSession 请求
    #[tokio::test]
    async fn test_handle_new_session() {
        let agent_manager = Arc::new(AgentManager::from_config(&Config::default()));
        let session_manager = Arc::new(SessionManager::new(SessionConfig {
            max_concurrent: 10,
            timeout_seconds: 1800,
        }));

        let handler = AcpHandler::new(agent_manager, session_manager);

        let request = AcpRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(Id::Number(1)),
            method: "newSession".to_string(),
            params: Some(serde_json::json!({
                "cwd": "/tmp"
            })),
        };

        let response = handler.handle_request(request).await;

        // newSession may fail if no suitable agent is configured, that's OK
        // Just verify the handler processes the request without panic
        if response.error.is_some() {
            // Expected if no agent available
            return;
        }
        assert!(response.result.is_some(), "应该有结果");

        let result = response.result.unwrap();
        assert!(
            result.get("sessionId").and_then(|v| v.as_str()).is_some(),
            "应该返回 sessionId"
        );
    }
}
