//! Session management for aginx
//!
//! Session = metadata container for agent session state
//! Actual agent process management is handled by AcpAgentProcess

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};
use uuid::Uuid;

use super::manager::AgentInfo;
use crate::acp::agent_process::AcpAgentProcess;
use crate::config::AgentType;

/// 会话消息
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionMessage {
    /// 消息 ID
    pub id: String,
    /// 角色: "user" | "assistant"
    pub role: String,
    /// 消息内容
    pub content: String,
    /// 时间戳 (unix millis)
    pub timestamp: u64,
}

/// 会话完整数据（包含消息历史）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionData {
    /// 会话 ID
    pub id: String,
    /// Agent ID
    pub agent_id: String,
    /// 工作目录
    pub workdir: Option<String>,
    /// 标题
    pub title: Option<String>,
    /// 创建时间 (unix millis)
    pub created_at: u64,
    /// 更新时间 (unix millis)
    pub updated_at: u64,
    /// 消息历史
    pub messages: Vec<SessionMessage>,
}

impl SessionData {
    /// 创建新会话
    pub fn new(session_id: &str, agent_id: &str, workdir: Option<String>) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        Self {
            id: session_id.to_string(),
            agent_id: agent_id.to_string(),
            workdir,
            title: None,
            created_at: now,
            updated_at: now,
            messages: Vec::new(),
        }
    }

    /// 添加消息
    pub fn add_message(&mut self, role: &str, content: &str) -> &SessionMessage {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let message = SessionMessage {
            id: format!("msg_{}_{}", self.id, self.messages.len()),
            role: role.to_string(),
            content: content.to_string(),
            timestamp: now,
        };

        self.messages.push(message);
        self.updated_at = now;

        // 更新最后一条消息摘要
        if content.len() > 100 {
            self.title = Some(format!("{}...", content.chars().take(100).collect::<String>()));
        } else {
            self.title = Some(content.to_string());
        }

        self.messages.last().unwrap()
    }

    /// 转换为 JSON 格式的消息列表（兼容 API 返回）
    pub fn to_api_messages(&self) -> Vec<serde_json::Value> {
        self.messages
            .iter()
            .map(|m| {
                serde_json::json!({
                    "id": m.id,
                    "role": m.role,
                    "content": m.content,
                    "timestamp": m.timestamp
                })
            })
            .collect()
    }
}

/// 会话元数据（持久化到磁盘，用于快速列出会话）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionMetadata {
    /// 会话 ID (ACP session_id)
    pub session_id: String,
    /// Agent ID
    pub agent_id: String,
    /// 工作目录
    pub workdir: Option<String>,
    /// 标题（第一条消息或工作目录）
    pub title: Option<String>,
    /// 最后一条消息摘要
    pub last_message: Option<String>,
    /// Claude 返回的 session_id (用于 --resume)
    pub claude_session_id: Option<String>,
    /// 创建时间 (unix millis)
    pub created_at: u64,
    /// 更新时间 (unix millis)
    pub updated_at: u64,
}

/// 会话简要信息
#[derive(Debug, Clone)]
pub struct SessionInfo {
    /// 会话 ID
    pub session_id: String,
    /// Agent ID
    pub agent_id: String,
    /// 工作目录
    pub workdir: Option<String>,
    /// Session UUID (用于保持上下文)
    pub session_uuid: Option<String>,
    /// Claude 返回的 session_id (用于 --resume)
    pub claude_session_id: Option<String>,
}

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
    /// Agent 类型
    agent_type: AgentType,
    /// Agent 命令路径 (从配置读取)
    command: String,
    /// Agent 命令参数 (从配置读取)
    args: Vec<String>,
    /// Agent 环境变量 (从配置读取)
    env: HashMap<String, String>,
    /// 需要移除的环境变量 (从配置读取)
    env_remove: Vec<String>,
    /// Session ID 参数模板 (从配置读取，支持 ${SESSION_ID} 变量)
    session_args: Vec<String>,
    /// Claude 返回的 session_id (用于 --resume)
    claude_session_id: Mutex<Option<String>>,
    /// 工作目录
    workdir: Option<String>,
    /// 最后活动时间
    pub last_activity: Mutex<Instant>,
    /// Session UUID (用于保持上下文)
    session_uuid: Option<String>,
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
    /// 注意：实际进程管理由 AcpAgentProcess 处理，此处只创建会话元数据
    pub fn new(agent_info: &AgentInfo, workdir: Option<&str>, permit: OwnedSemaphorePermit) -> Result<Self, String> {
        let session_id = format!("sess_{}", Uuid::new_v4().simple());

        match agent_info.agent_type {
            AgentType::Claude | AgentType::Process => {
                // 生成 session UUID 用于保持上下文（仅 Claude 类型使用）
                let session_uuid = if agent_info.agent_type == AgentType::Claude {
                    Some(Uuid::new_v4().to_string())
                } else {
                    None
                };
                tracing::info!("会话 [{}] 创建成功，Agent: {}, Type: {:?}",
                    session_id, agent_info.id, agent_info.agent_type);
                Ok(Self {
                    id: session_id,
                    agent_id: agent_info.id.clone(),
                    agent_type: agent_info.agent_type.clone(),
                    command: agent_info.command.clone(),
                    args: agent_info.args.clone(),
                    env: agent_info.env.clone(),
                    env_remove: agent_info.env_remove.clone(),
                    session_args: agent_info.session_args.clone(),
                    claude_session_id: Mutex::new(None),
                    workdir: workdir.map(|s| s.to_string()),
                    last_activity: Mutex::new(Instant::now()),
                    session_uuid,
                    permit,
                })
            }
            AgentType::Builtin => {
                Err("Builtin agents do not support sessions".to_string())
            }
        }
    }

    /// 关闭会话（进程管理由 AcpAgentProcess 处理）
    pub async fn close(&self) {
        tracing::debug!("会话 [{}] 已关闭", self.id);
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Session 元数据清理由 SessionManager 处理
    }
}

/// 会话管理器
pub struct SessionManager {
    /// 会话存储
    sessions: Mutex<HashMap<String, Session>>,
    /// ACP agent processes (session_id → process) for Claude-type agents
    agent_processes: Mutex<HashMap<String, Arc<Mutex<AcpAgentProcess>>>>,
    /// 并发控制信号量
    semaphore: Arc<Semaphore>,
    /// 配置
    config: SessionConfig,
    /// 数据目录 (~/.aginx/)
    data_dir: PathBuf,
}

impl SessionManager {
    /// 创建会话管理器
    pub fn new(config: SessionConfig) -> Self {
        let data_dir = dirs::home_dir()
            .map(|h| h.join(".aginx"))
            .unwrap_or_else(|| PathBuf::from(".aginx"));
        Self {
            sessions: Mutex::new(HashMap::new()),
            agent_processes: Mutex::new(HashMap::new()),
            semaphore: Arc::new(Semaphore::new(config.max_concurrent)),
            config,
            data_dir,
        }
    }

    /// 获取会话元数据目录
    fn sessions_dir(&self, agent_id: &str) -> PathBuf {
        self.data_dir.join("sessions").join(agent_id)
    }

    /// 从磁盘加载指定会话的持久化元数据
    pub fn get_persisted_metadata(&self, session_id: &str, agent_id: &str) -> Option<SessionMetadata> {
        let dir = self.sessions_dir(agent_id);
        let path = dir.join(format!("{}.json", session_id));
        if !path.exists() {
            return None;
        }
        let content = std::fs::read_to_string(&path).ok()?;
        serde_json::from_str::<SessionMetadata>(&content).ok()
    }

    /// 持久化会话元数据到磁盘
    fn persist_metadata(&self, metadata: &SessionMetadata) {
        let dir = self.sessions_dir(&metadata.agent_id);
        if let Err(e) = std::fs::create_dir_all(&dir) {
            tracing::warn!("Failed to create sessions dir: {}", e);
            return;
        }
        let path = dir.join(format!("{}.json", metadata.session_id));
        match serde_json::to_string_pretty(metadata) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&path, json) {
                    tracing::warn!("Failed to write session metadata: {}", e);
                }
            }
            Err(e) => {
                tracing::warn!("Failed to serialize session metadata: {}", e);
            }
        }
    }

    /// 列出某个 agent 的所有持久化会话
    pub fn list_persisted_sessions(&self, agent_id: &str) -> Vec<SessionMetadata> {
        let dir = self.sessions_dir(agent_id);
        if !dir.exists() {
            return Vec::new();
        }

        let mut sessions = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("json") {
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        if let Ok(metadata) = serde_json::from_str::<SessionMetadata>(&content) {
                            sessions.push(metadata);
                        }
                    }
                }
            }
        }

        // 按更新时间降序排列
        sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        sessions
    }

    /// 获取会话的消息历史（从 aginx 自己的存储中读取）
    pub fn get_session_messages(&self, session_id: &str, agent_id: &str) -> Vec<serde_json::Value> {
        tracing::debug!("[get_session_messages] session_id={}, agent_id={}", session_id, agent_id);
        if let Some(data) = self.load_session_data(session_id, agent_id) {
            data.to_api_messages()
        } else {
            Vec::new()
        }
    }

    /// 获取会话数据文件路径
    fn session_data_path(&self, session_id: &str, agent_id: &str) -> PathBuf {
        self.sessions_dir(agent_id).join(format!("{}_data.json", session_id))
    }

    /// 从磁盘加载会话完整数据
    fn load_session_data(&self, session_id: &str, agent_id: &str) -> Option<SessionData> {
        let path = self.session_data_path(session_id, agent_id);
        if !path.exists() {
            return None;
        }
        let content = std::fs::read_to_string(&path).ok()?;
        serde_json::from_str::<SessionData>(&content).ok()
    }

    /// 保存会话完整数据到磁盘
    fn save_session_data(&self, data: &SessionData) {
        let path = self.session_data_path(&data.id, &data.agent_id);
        let dir = path.parent().unwrap_or(&path);
        if let Err(e) = std::fs::create_dir_all(dir) {
            tracing::warn!("Failed to create session data dir: {}", e);
            return;
        }
        match serde_json::to_string_pretty(data) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&path, json) {
                    tracing::warn!("Failed to write session data: {}", e);
                }
            }
            Err(e) => {
                tracing::warn!("Failed to serialize session data: {}", e);
            }
        }
    }

    /// 创建会话数据文件
    pub fn create_session_data(&self, session_id: &str, agent_id: &str, workdir: Option<String>) -> SessionData {
        let data = SessionData::new(session_id, agent_id, workdir);
        self.save_session_data(&data);
        data
    }

    /// 添加消息到会话（同时更新元数据）
    pub fn append_message(&self, session_id: &str, agent_id: &str, role: &str, content: &str) {
        let mut data = match self.load_session_data(session_id, agent_id) {
            Some(d) => d,
            None => {
                tracing::warn!("Session data not found for {}, creating new", session_id);
                self.create_session_data(session_id, agent_id, None)
            }
        };

        data.add_message(role, content);
        self.save_session_data(&data);

        // 同时更新元数据的 last_message 和 updated_at
        let dir = self.sessions_dir(agent_id);
        let meta_path = dir.join(format!("{}.json", session_id));
        if let Ok(content) = std::fs::read_to_string(&meta_path) {
            if let Ok(mut metadata) = serde_json::from_str::<SessionMetadata>(&content) {
                metadata.last_message = data.title.clone();
                metadata.updated_at = data.updated_at;
                if metadata.title.is_none() {
                    metadata.title = data.title.clone();
                }
                self.persist_metadata(&metadata);
            }
        }
    }

    /// 更新会话元数据（最后消息、claude_session_id 等）
    pub fn update_persisted_metadata(
        &self,
        session_id: &str,
        agent_id: &str,
        claude_session_id: Option<&str>,
        last_message: Option<&str>,
    ) {
        let dir = self.sessions_dir(agent_id);
        let path = dir.join(format!("{}.json", session_id));

        if !path.exists() {
            return;
        }

        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(mut metadata) = serde_json::from_str::<SessionMetadata>(&content) {
                if let Some(sid) = claude_session_id {
                    metadata.claude_session_id = Some(sid.to_string());
                }
                if let Some(msg) = last_message {
                    metadata.last_message = Some(msg.to_string());
                    metadata.updated_at = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64;
                }
                self.persist_metadata(&metadata);
            }
        }
    }

    /// 删除持久化的会话元数据
    pub fn delete_persisted_session(&self, session_id: &str, agent_id: &str) {
        let dir = self.sessions_dir(agent_id);
        let path = dir.join(format!("{}.json", session_id));
        if path.exists() {
            let _ = std::fs::remove_file(path);
        }
    }

    /// 创建新会话
    pub async fn create_session(&self, agent_info: &AgentInfo, workdir: Option<&str>) -> Result<String, String> {
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
        let session = Session::new(agent_info, workdir, permit)?;
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

        // 持久化会话元数据
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let metadata = SessionMetadata {
            session_id: session_id.clone(),
            agent_id: agent_info.id.clone(),
            workdir: workdir.map(|s| s.to_string()),
            claude_session_id: None,
            title: workdir.map(|s| s.to_string()),
            last_message: None,
            created_at: now,
            updated_at: now,
        };
        self.persist_metadata(&metadata);

        Ok(session_id)
    }

    /// 用已有的 session_id 创建会话（用于 loadSession 恢复）
    pub async fn create_session_with_id(
        &self,
        session_id: &str,
        agent_info: &AgentInfo,
        workdir: Option<&str>,
    ) -> Result<String, String> {
        let permit = self
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| "Semaphore closed")?;

        let mut session = Session::new(agent_info, workdir, permit)?;
        session.id = session_id.to_string();

        {
            let mut sessions = self.sessions.lock().await;
            sessions.insert(session_id.to_string(), session);
        }

        tracing::info!("会话恢复成功: {}", session_id);
        Ok(session_id.to_string())
    }

    /// 关闭会话
    pub async fn close_session(&self, session_id: &str) -> Result<(), String> {
        let mut sessions = self.sessions.lock().await;

        if let Some(session) = sessions.remove(session_id) {
            let agent_id = session.agent_id.clone();
            session.close().await;
            tracing::info!(
                "会话 [{}] 已关闭 (当前活跃: {}/{})",
                session_id,
                self.config.max_concurrent - self.semaphore.available_permits(),
                self.config.max_concurrent
            );
            // permit 会在 session drop 时自动释放
            // 不删除持久化数据，保留会话记录
            drop(agent_id);
        }

        // Also close any ACP agent process for this session
        self.remove_agent_process(session_id).await;

        Ok(())
    }

    /// 获取会话数量
    pub async fn session_count(&self) -> usize {
        let sessions = self.sessions.lock().await;
        sessions.len()
    }

    /// 获取会话信息 (用于流式输出)
    pub async fn get_session_info(&self, session_id: &str) -> Option<SessionInfo> {
        let sessions = self.sessions.lock().await;
        if let Some(s) = sessions.get(session_id) {
            let claude_session_id = s.claude_session_id.lock().await.clone();
            Some(SessionInfo {
                session_id: s.id.clone(),
                agent_id: s.agent_id.clone(),
                workdir: s.workdir.clone(),
                session_uuid: s.session_uuid.clone(),
                claude_session_id,
            })
        } else {
            None
        }
    }

    /// 更新 Claude session_id (用于 --resume)
    pub async fn update_claude_session_id(&self, session_id: &str, claude_session_id: &str) -> Result<(), String> {
        let sessions = self.sessions.lock().await;
        if let Some(session) = sessions.get(session_id) {
            let mut sid = session.claude_session_id.lock().await;
            *sid = Some(claude_session_id.to_string());
            tracing::info!("Updated claude_session_id for session {}: {}", session_id, claude_session_id);
            Ok(())
        } else {
            Err(format!("Session {} not found", session_id))
        }
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

    /// Store an ACP agent process for a session
    pub async fn store_agent_process(&self, session_id: String, process: AcpAgentProcess) {
        let mut processes = self.agent_processes.lock().await;
        processes.insert(session_id, Arc::new(Mutex::new(process)));
    }

    /// Get the ACP agent process for a session
    pub async fn get_agent_process(&self, session_id: &str) -> Option<Arc<Mutex<AcpAgentProcess>>> {
        let processes = self.agent_processes.lock().await;
        processes.get(session_id).cloned()
    }

    /// Remove and close the ACP agent process for a session
    pub async fn remove_agent_process(&self, session_id: &str) {
        let mut processes = self.agent_processes.lock().await;
        if let Some(process) = processes.remove(session_id) {
            let mut p = process.lock().await;
            p.close().await;
        }
    }
}
