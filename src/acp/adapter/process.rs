//! 通用进程 adapter — stdin/stdout 管道模式
//!
//! 适用于任何命令行程序：将 message 写入 stdin，逐行读取 stdout，
//! 每行作为 agent_message_chunk 通知发送。

use std::time::Duration;
use tokio::sync::mpsc;

use crate::acp::adapter::PromptAdapter;
use crate::acp::types::{AcpResponse, Id, PromptResult, StopReason};
use crate::agent::AgentInfo;

/// 通用进程 adapter
pub struct ProcessAdapter {
    command: String,
    args_template: Vec<String>,
    env: std::collections::HashMap<String, String>,
    timeout_secs: u64,
}

impl ProcessAdapter {
    pub fn new(agent_info: &AgentInfo) -> Self {
        Self {
            command: agent_info.command.clone(),
            args_template: agent_info.args.clone(),
            env: agent_info.env.clone(),
            timeout_secs: agent_info.timeout.unwrap_or(60),
        }
    }
}

#[async_trait::async_trait]
impl PromptAdapter for ProcessAdapter {
    async fn send_request(
        &self,
        _method: &str,
        _params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        Err("Process adapter does not support ACP passthrough".to_string())
    }

    async fn send_streaming_request(
        &self,
        _method: &str,
        _params: serde_json::Value,
        _tx: Option<mpsc::Sender<String>>,
    ) -> Result<serde_json::Value, String> {
        Err("Process adapter does not support ACP passthrough".to_string())
    }

    async fn prompt(
        &self,
        session_id: &str,
        _message: &str,
        request_id: Option<Id>,
        tx: mpsc::Sender<String>,
    ) -> Result<(), String> {
        let command = self.command.clone();
        let session_id = session_id.to_string();
        let message = _message.to_string();
        let args: Vec<String> = self.args_template.iter()
            .map(|arg| arg.replace("${SESSION_ID}", &session_id))
            .collect();
        let timeout_secs = self.timeout_secs;
        let env = self.env.clone();

        tokio::spawn(async move {
            let mut cmd = tokio::process::Command::new(&command);
            cmd.args(&args)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());

            for (k, v) in &env {
                cmd.env(k, v);
            }

            match cmd.spawn() {
                Ok(mut child) => {
                    // Write message to stdin
                    if let Some(mut stdin) = child.stdin.take() {
                        use tokio::io::AsyncWriteExt;
                        let _ = stdin.write_all(message.as_bytes()).await;
                        let _ = stdin.write_all(b"\n").await;
                        drop(stdin);
                    }

                    // Capture stderr in background
                    let stderr_handle = child.stderr.take().map(|stderr| {
                        tokio::spawn(async move {
                            use tokio::io::AsyncBufReadExt;
                            let reader = tokio::io::BufReader::new(stderr);
                            let mut lines = reader.lines();
                            let mut stderr_output = String::new();
                            while let Ok(Some(line)) = lines.next_line().await {
                                tracing::debug!("Process stderr: {}", line);
                                stderr_output.push_str(&line);
                                stderr_output.push('\n');
                            }
                            stderr_output
                        })
                    });

                    // Read stdout with timeout
                    let read_stdout = async {
                        if let Some(stdout) = child.stdout.take() {
                            use tokio::io::AsyncBufReadExt;
                            let reader = tokio::io::BufReader::new(stdout);
                            let mut lines = reader.lines();

                            while let Ok(Some(line)) = lines.next_line().await {
                                let notification = serde_json::json!({
                                    "jsonrpc": "2.0",
                                    "method": "session/update",
                                    "params": {
                                        "sessionId": session_id,
                                        "update": {
                                            "sessionUpdate": "agent_message_chunk",
                                            "content": {"type": "text", "text": line}
                                        }
                                    }
                                });
                                if tx.send(serde_json::to_string(&notification).unwrap_or_default()).await.is_err() {
                                    break;
                                }
                            }
                        }
                    };

                    let timed_out = match tokio::time::timeout(Duration::from_secs(timeout_secs), read_stdout).await {
                        Ok(_) => false,
                        Err(_) => {
                            tracing::warn!("Process timed out after {}s, killing", timeout_secs);
                            let _ = child.kill().await;
                            true
                        }
                    };

                    let exit_status = child.wait().await.ok();
                    let stderr_output = if let Some(h) = stderr_handle {
                        h.await.unwrap_or_default()
                    } else {
                        String::new()
                    };

                    let exit_code = exit_status.as_ref().and_then(|s| s.code());

                    if timed_out {
                        let error_response = AcpResponse::error(request_id, -32603, &format!("Process timed out after {}s", timeout_secs));
                        let _ = tx.send(serde_json::to_string(&error_response).unwrap_or_default()).await;
                    } else if exit_code.map_or(false, |c| c != 0) {
                        let error_detail = if stderr_output.is_empty() {
                            format!("Process exited with code {}", exit_code.unwrap_or(-1))
                        } else {
                            format!("Process exited with code {}: {}", exit_code.unwrap_or(-1), stderr_output.trim())
                        };
                        let error_response = AcpResponse::error(request_id, -32603, &error_detail);
                        let _ = tx.send(serde_json::to_string(&error_response).unwrap_or_default()).await;
                    } else {
                        let final_response = AcpResponse::success(request_id, PromptResult { stopReason: StopReason::EndTurn });
                        let _ = tx.send(serde_json::to_string(&final_response).unwrap_or_default()).await;
                    }
                }
                Err(e) => {
                    let error_response = AcpResponse::error(request_id, -32603, &format!("Failed to start process: {}", e));
                    let _ = tx.send(serde_json::to_string(&error_response).unwrap_or_default()).await;
                }
            }
        });

        Ok(())
    }
}
