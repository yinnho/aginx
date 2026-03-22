//! Session management for aginx
//!
//! 会话 = 一个长期运行的 agent 进程
//! 同一个会话内的多次消息在同一个进程里处理，保持上下文

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};
use uuid::Uuid;

use super::manager::AgentInfo;
use crate::config::AgentType;

/// 会话配置
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// 最大并发会话数
    pub max_concurrent: usize,
    /// 会话超时（秒）
    pub timeout_seconds: u64,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            max_concurrent: 10,
            timeout_seconds: 1800, // 30分钟
        }
    }
}

/// 会话状态
pub struct Session {
    /// 会话 ID
    pub id: String,
    /// Agent ID
    pub agent_id: String,
    /// 进程 stdin 写入端
    stdin: Mutex<std::process::ChildStdin>,
    /// 进程 stdout 读取端
    stdout: Mutex<BufReader<std::process::ChildStdout>>,
    /// 最后活动时间
    pub last_activity: Mutex<Instant>,
    /// 进程句柄（用于关闭时 kill）
    process: Mutex<Option<std::process::Child>>,
    /// 信号量许可（持有它来限制并发）
    #[allow(dead_code)]
    permit: OwnedSemaphorePermit,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("id", &self.id)
            .field("agent_id", &self.agent_id)
            .finish()
    }
}

impl Session {
    /// 创建新会话
    pub fn new(agent_info: &AgentInfo, permit: OwnedSemaphorePermit) -> Result<Self, String> {
        let session_id = format!("sess_{}", Uuid::new_v4().simple());

        let (stdin, stdout, process) = match agent_info.agent_type {
            AgentType::Claude => Self::spawn_claude()?,
            AgentType::Process => Self::spawn_process(agent_info)?,
            AgentType::Builtin => {
                return Err("Builtin agents do not support sessions".to_string());
            }
        };

        tracing::info!("会话 [{}] 创建成功，Agent: {}", session_id, agent_info.id);

        Ok(Self {
            id: session_id,
            agent_id: agent_info.id.clone(),
            stdin: Mutex::new(stdin),
            stdout: Mutex::new(stdout),
            last_activity: Mutex::new(Instant::now()),
            process: Mutex::new(Some(process)),
            permit,
        })
    }

    /// 启动 Claude 进程
    fn spawn_claude() -> Result<
        (
            std::process::ChildStdin,
            BufReader<std::process::ChildStdout>,
            std::process::Child,
        ),
        String,
    > {
        let mut cmd = Command::new("claude");
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env_remove("CLAUDECODE"); // 防止嵌套会话

        let mut child = cmd.spawn().map_err(|e| format!("Failed to spawn claude: {}", e))?;

        let stdin = child.stdin.take().ok_or("Failed to get stdin")?;
        let stdout = child.stdout.take().ok_or("Failed to get stdout")?;
        let reader = BufReader::new(stdout);

        Ok((stdin, reader, child))
    }

    /// 启动外部进程
    fn spawn_process(agent_info: &AgentInfo) -> Result<
        (
            std::process::ChildStdin,
            BufReader<std::process::ChildStdout>,
            std::process::Child,
        ),
        String,
    > {
        if agent_info.command.is_empty() {
            return Err(format!("Agent {} has no command configured", agent_info.id));
        }

        let mut cmd = Command::new(&agent_info.command);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // 添加参数
        for arg in &agent_info.args {
            cmd.arg(arg);
        }

        // 设置工作目录
        if let Some(ref dir) = agent_info.working_dir {
            cmd.current_dir(dir);
        }

        // 设置环境变量
        for (key, value) in &agent_info.env {
            cmd.env(key, value);
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn {}: {}", agent_info.command, e))?;

        let stdin = child.stdin.take().ok_or("Failed to get stdin")?;
        let stdout = child.stdout.take().ok_or("Failed to get stdout")?;
        let reader = BufReader::new(stdout);

        Ok((stdin, reader, child))
    }

    /// 发送消息并获取响应
    pub async fn send_message(&self, message: &str) -> Result<String, String> {
        // 更新活动时间
        {
            let mut last = self.last_activity.lock().await;
            *last = Instant::now();
        }

        // 写入消息
        {
            let mut stdin = self.stdin.lock().await;
            writeln!(stdin, "{}", message).map_err(|e| format!("Write error: {}", e))?;
        }

        // 读取响应
        let mut response = String::new();
        {
            let mut stdout = self.stdout.lock().await;
            match stdout.read_line(&mut response) {
                Ok(0) => return Err("Process closed".to_string()),
                Ok(_) => {}
                Err(e) => return Err(format!("Read error: {}", e)),
            }
        }

        Ok(response)
    }

    /// 关闭会话
    pub async fn close(&self) {
        let mut process = self.process.lock().await;
        if let Some(mut child) = process.take() {
            let _ = child.kill();
            tracing::info!("会话 [{}] 已关闭", self.id);
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // 尝试同步关闭进程
        if let Ok(mut process) = self.process.try_lock() {
            if let Some(mut child) = process.take() {
                let _ = child.kill();
            }
        }
    }
}

/// 会话管理器
pub struct SessionManager {
    /// 会话存储
    sessions: Mutex<HashMap<String, Session>>,
    /// 并发控制信号量
    semaphore: Arc<Semaphore>,
    /// 配置
    config: SessionConfig,
}

impl SessionManager {
    /// 创建会话管理器
    pub fn new(config: SessionConfig) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            semaphore: Arc::new(Semaphore::new(config.max_concurrent)),
            config,
        }
    }

    /// 创建新会话
    pub async fn create_session(&self, agent_info: &AgentInfo) -> Result<String, String> {
        // 获取可用槽位数
        let available = self.semaphore.available_permits();
        tracing::info!("尝试创建会话，可用槽位: {}", available);

        // 尝试获取信号量（会阻塞直到有空位）
        let permit = self
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| "Semaphore closed")?;

        // 创建会话
        let session = Session::new(agent_info, permit)?;
        let session_id = session.id.clone();

        // 存储会话
        {
            let mut sessions = self.sessions.lock().await;
            sessions.insert(session_id.clone(), session);
        }

        tracing::info!(
            "会话创建成功: {} (当前活跃: {}/{})",
            session_id,
            self.config.max_concurrent - self.semaphore.available_permits(),
            self.config.max_concurrent
        );

        Ok(session_id)
    }

    /// 向会话发送消息
    pub async fn send_message(&self, session_id: &str, message: &str) -> Result<String, String> {
        let sessions = self.sessions.lock().await;

        let session = sessions
            .get(session_id)
            .ok_or_else(|| format!("Session not found: {}", session_id))?;

        session.send_message(message).await
    }

    /// 关闭会话
    pub async fn close_session(&self, session_id: &str) -> Result<(), String> {
        let mut sessions = self.sessions.lock().await;

        if let Some(session) = sessions.remove(session_id) {
            session.close().await;
            tracing::info!(
                "会话 [{}] 已关闭 (当前活跃: {}/{})",
                session_id,
                self.config.max_concurrent - self.semaphore.available_permits(),
                self.config.max_concurrent
            );
            // permit 会在 session drop 时自动释放
        }

        Ok(())
    }

    /// 获取会话数量
    pub async fn session_count(&self) -> usize {
        let sessions = self.sessions.lock().await;
        sessions.len()
    }

    /// 清理超时会话
    pub async fn cleanup_timeout_sessions(&self) -> usize {
        let timeout = Duration::from_secs(self.config.timeout_seconds);
        let mut sessions = self.sessions.lock().await;
        let mut removed = 0;

        let timeout_ids: Vec<String> = sessions
            .iter()
            .filter_map(|(id, session)| {
                if let Ok(last) = session.last_activity.try_lock() {
                    if last.elapsed() > timeout {
                        return Some(id.clone());
                    }
                }
                None
            })
            .collect();

        for id in timeout_ids {
            if let Some(session) = sessions.remove(&id) {
                // 异步关闭需要 detach，但我们在这里直接 drop
                drop(session);
                removed += 1;
                tracing::info!("会话 [{}] 超时已清理", id);
            }
        }

        if removed > 0 {
            tracing::info!("清理了 {} 个超时会话", removed);
        }

        removed
    }

    /// 启动超时清理任务
    pub fn start_cleanup_task(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            loop {
                interval.tick().await;
                self.cleanup_timeout_sessions().await;
            }
        })
    }
}
