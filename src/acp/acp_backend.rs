//! ACP 原生后端 — 通过 stdin/stdout 与 agent 进程通信 ACP 协议
//!
//! 这是 aginx 的标准 agent 接入方式。agent 进程在 stdin/stdout 上
//! 说 ACP（JSON-RPC 2.0 ndjson），aginx 纯透传。

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use async_trait::async_trait;
use anyhow::{Result, anyhow};
use serde_json::{json, Value};

use super::backend::AgentBackend;
use crate::agent::AgentInfo;

/// ACP 原生后端 — spawn agent 进程，通过 ACP JSON-RPC 通信
pub struct AcpBackend {
    agent_info: AgentInfo,
    /// session_id → 进程管理器
    processes: Mutex<HashMap<String, Arc<Mutex<AcpProcess>>>>,
}

/// 单个 agent 进程的 ACP 连接
struct AcpProcess {
    #[allow(dead_code)]
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    /// 自增请求 ID
    next_id: u64,
}

impl AcpProcess {
    /// spawn agent 进程并进行 ACP initialize 握手
    async fn spawn(agent_info: &AgentInfo, workdir: Option<&str>) -> Result<Self> {
        let mut cmd = tokio::process::Command::new(&agent_info.command);
        cmd.args(&agent_info.args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        if let Some(dir) = workdir {
            cmd.current_dir(dir);
        }
        for (k, v) in &agent_info.env {
            cmd.env(k, v);
        }

        let mut child = cmd.spawn()
            .map_err(|e| anyhow!("Failed to spawn agent '{}': {}", agent_info.id, e))?;

        let stdin = child.stdin.take().ok_or_else(|| anyhow!("Failed to get stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| anyhow!("Failed to get stdout"))?;

        // 发送 ACP initialize
        let init_req = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": 1,
                "clientInfo": {
                    "name": "aginx",
                    "version": "0.1.0"
                }
            }
        });

        use tokio::io::{AsyncWriteExt, AsyncBufReadExt, BufReader};
        {
            let mut stdin = stdin;
            let line = serde_json::to_string(&init_req)?;
            stdin.write_all(line.as_bytes()).await?;
            stdin.write_all(b"\n").await?;
            stdin.flush().await?;

            // 读 initialize 响应
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            let response_line = lines.next_line().await?
                .ok_or_else(|| anyhow!("Agent closed stdout during initialize"))?;

            let response: Value = serde_json::from_str(&response_line)?;
            if let Some(error) = response.get("error") {
                return Err(anyhow!("Agent initialize failed: {}", error));
            }

            tracing::info!("ACP agent '{}' initialized", agent_info.id);

            // 需要把 stdout reader 保存起来用于后续读取
            // 简化方案：先把 stdin 保存，后续 prompt 时再 spawn reader task
            // 注意：stdout 被 lines() 消费了，需要重新获取...
            // TODO: 需要更好的进程生命周期管理

            // 暂时方案：保存 stdin，prompt 时 spawn 新的 reader
            drop(stdin);
        }

        // FIXME: stdout 已经被消费了，这里有问题
        // 暂时返回错误，后续实现完整的进程管理
        Err(anyhow!("AcpBackend not fully implemented yet"))
    }
}

#[async_trait]
impl AgentBackend for AcpBackend {
    async fn create_session(
        &self,
        session_id: &str,
        agent_id: &str,
        workdir: Option<&str>,
    ) -> Result<String> {
        // spawn agent 进程
        let process = AcpProcess::spawn(&self.agent_info, workdir).await?;
        let mut processes = self.processes.lock().await;
        processes.insert(session_id.to_string(), Arc::new(Mutex::new(process)));

        // 发送 session/new
        // TODO: 通过进程发送 ACP session/new

        Ok(session_id.to_string())
    }

    async fn load_session(
        &self,
        _session_id: &str,
        _agent_session_id: Option<&str>,
        _workdir: Option<&str>,
    ) -> Result<()> {
        // TODO: 发送 ACP session/load
        Ok(())
    }

    async fn prompt(
        &self,
        session_id: &str,
        message: &str,
        tx: mpsc::Sender<String>,
    ) -> Result<()> {
        // TODO: 发送 ACP session/prompt，读取通知并转发到 tx
        Err(anyhow!("AcpBackend::prompt not fully implemented yet"))
    }

    async fn cancel(&self, session_id: &str) -> Result<()> {
        let mut processes = self.processes.lock().await;
        if let Some(process) = processes.remove(session_id) {
            let mut p = process.lock().await;
            let _ = p.child.kill().await;
        }
        Ok(())
    }

    async fn close_session(&self, session_id: &str) -> Result<()> {
        self.cancel(session_id).await
    }

    async fn list_conversations(&self, _agent_id: &str) -> Result<Vec<Value>> {
        // ACP agent 暂时通过 aginx 自己的持久化存储
        Ok(vec![])
    }

    async fn get_messages(
        &self,
        _session_id: &str,
        _agent_id: &str,
        _limit: u32,
    ) -> Result<Vec<Value>> {
        Ok(vec![])
    }

    async fn delete_conversation(&self, _session_id: &str, _agent_id: &str) -> Result<()> {
        Ok(())
    }
}

impl AcpBackend {
    pub fn new(agent_info: AgentInfo) -> Self {
        Self {
            agent_info,
            processes: Mutex::new(HashMap::new()),
        }
    }
}
