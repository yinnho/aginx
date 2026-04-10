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
    /// Agent ID
    pub agent_id: String,
    /// 工作目录
    pub workdir: Option<String>,
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
    /// Claude 返回的 session_id (用于 --resume)
    claude_session_id: Mutex<Option<String>>,
    /// 工作目录
    workdir: Option<String>,
    /// 最后活动时间
    pub last_activity: Mutex<Instant>,
    /// 信号量许可（持有它来限制并发）
    #[allow(dead_code)] // RAII: held to keep semaphore permit alive
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
                tracing::info!("会话 [{}] 创建成功，Agent: {}, Type: {:?}",
                    session_id, agent_info.id, agent_info.agent_type);
                Ok(Self {
                    id: session_id,
                    agent_id: agent_info.id.clone(),
                    claude_session_id: Mutex::new(None),
                    workdir: workdir.map(|s| s.to_string()),
                    last_activity: Mutex::new(Instant::now()),
                    permit,
                })
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
        let data_dir = crate::config::data_dir();
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

    /// Find the working directory for a Claude session by scanning its JSONL file
    pub fn find_workdir_for_claude_session(&self, claude_session_id: &str) -> Option<String> {
        let claude_projects_dir = dirs::home_dir()
            .map(|h| h.join(".claude").join("projects"))?;

        let path = self.find_claude_jsonl_path(claude_session_id, &claude_projects_dir)?;
        let file = std::fs::File::open(&path).ok()?;
        let reader = std::io::BufReader::new(file);

        for (i, line) in std::io::BufRead::lines(reader).enumerate() {
            if i >= 30 { break; }
            let line = match line {
                Ok(l) => l,
                Err(_) => break,
            };
            let line = line.trim();
            if line.is_empty() { continue; }

            if let Ok(event) = serde_json::from_str::<serde_json::Value>(line) {
                if event.get("type").and_then(|t| t.as_str()) == Some("user") {
                    if let Some(cwd) = event.get("cwd").and_then(|c| c.as_str()) {
                        return Some(cwd.to_string());
                    }
                }
            }
        }
        None
    }

    /// Create a session using the Claude session ID directly (for --resume support)
    pub async fn create_session_with_claude_id(
        &self,
        claude_session_id: &str,
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
        session.id = claude_session_id.to_string();

        // Store claude session ID for --resume
        {
            let mut sid = session.claude_session_id.lock().await;
            *sid = Some(claude_session_id.to_string());
        }

        {
            let mut sessions = self.sessions.lock().await;
            sessions.insert(claude_session_id.to_string(), session);
        }

        tracing::info!("Session created with claude ID: {} (will use --resume)", claude_session_id);
        Ok(claude_session_id.to_string())
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

    /// 获取会话信息 (用于流式输出)
    pub async fn get_session_info(&self, session_id: &str) -> Option<SessionInfo> {
        let sessions = self.sessions.lock().await;
        if let Some(s) = sessions.get(session_id) {
            let claude_session_id = s.claude_session_id.lock().await.clone();
            Some(SessionInfo {
                agent_id: s.agent_id.clone(),
                workdir: s.workdir.clone(),
                claude_session_id,
            })
        } else {
            None
        }
    }

    /// 更新会话的最后活动时间（防止超时清理）
    pub async fn touch_session(&self, session_id: &str) {
        let sessions = self.sessions.lock().await;
        if let Some(session) = sessions.get(session_id) {
            let mut last = session.last_activity.lock().await;
            *last = Instant::now();
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
        let sessions = self.sessions.lock().await;
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

        // Release sessions lock before removing agent processes (which also takes locks)
        drop(sessions);

        for id in timeout_ids {
            // Remove and close the agent process first
            self.remove_agent_process(&id).await;

            // Re-check activity before removing (session may have been touched)
            let mut sessions = self.sessions.lock().await;
            if let Some(session) = sessions.get(&id) {
                if let Ok(last) = session.last_activity.try_lock() {
                    if last.elapsed() <= timeout {
                        continue; // Re-activated since scan, skip
                    }
                }
            }
            if let Some(session) = sessions.remove(&id) {
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

    // ========== Claude CLI Session Scanning ==========

    // ========== Direct Claude Session Scanning (no aginx metadata) ==========

    /// Find the JSONL file path in a Claude session by searching project directories
    fn find_claude_jsonl_path(
        &self,
        claude_session_id: &str,
        claude_projects_dir: &std::path::Path,
    ) -> Option<PathBuf> {
        let filename = format!("{}.jsonl", claude_session_id);

        if let Ok(entries) = std::fs::read_dir(claude_projects_dir) {
            for entry in entries.flatten() {
                let project_dir = entry.path();
                if !project_dir.is_dir() { continue; }
                let candidate = project_dir.join(&filename);
                if candidate.exists() {
                    return Some(candidate);
                }
            }
        }

        None
    }

    /// Parse metadata from a Claude JSONL file (lightweight - only read first ~30 lines)
    fn parse_jsonl_metadata(
        &self,
        path: &std::path::Path,
        claude_session_id: &str,
    ) -> Option<DiscoveredClaudeSession> {
        let file = std::fs::File::open(path).ok()?;
        let reader = std::io::BufReader::new(file);

        let mut first_user_message: Option<String> = None;
        let mut last_assistant_text: Option<String> = None;
        let mut first_timestamp: Option<u64> = None;
        let mut cwd: Option<String> = None;

        for (i, line) in std::io::BufRead::lines(reader).enumerate() {
            if i >= 30 { break; }
            let line = match line {
                Ok(l) => l,
                Err(_) => break,
            };
            let line = line.trim();
            if line.is_empty() { continue; }

            if let Ok(event) = serde_json::from_str::<serde_json::Value>(line) {
                let event_type = event.get("type").and_then(|t| t.as_str());

                match event_type {
                    Some("queue-operation") => {
                        if first_user_message.is_none() {
                            if let Some(content) = event.get("content").and_then(|c| c.as_str()) {
                                let truncated: String = content.chars().take(100).collect();
                                first_user_message = Some(truncated);
                            }
                        }
                        if first_timestamp.is_none() {
                            first_timestamp = event.get("timestamp")
                                .and_then(|t| t.as_str())
                                .and_then(|ts| parse_iso_timestamp(ts));
                        }
                    }
                    Some("user") => {
                        if cwd.is_none() {
                            cwd = event.get("cwd").and_then(|c| c.as_str()).map(String::from);
                        }
                        if first_user_message.is_none() {
                            if let Some(msg) = event.get("message") {
                                let content_val = msg.get("content");
                                if let Some(text) = content_val.and_then(|c| c.as_str()) {
                                    if !text.contains("<local-command-caveat>")
                                        && !text.contains("<command-name>")
                                        && !text.contains("This session is being continued")
                                        && !text.contains("<local-command-stdout>")
                                    {
                                        let truncated: String = text.chars().take(100).collect();
                                        first_user_message = Some(truncated);
                                    }
                                }
                            }
                        }
                        if first_timestamp.is_none() {
                            first_timestamp = event.get("timestamp")
                                .and_then(|t| t.as_str())
                                .and_then(|ts| parse_iso_timestamp(ts));
                        }
                    }
                    Some("assistant") => {
                        if let Some(msg) = event.get("message") {
                            if let Some(content_blocks) = msg.get("content").and_then(|c| c.as_array()) {
                                for block in content_blocks.iter().rev() {
                                    if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                                        if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                                            if !text.is_empty() {
                                                let truncated: String = text.chars().take(100).collect();
                                                last_assistant_text = Some(truncated);
                                                break;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        let file_modified = std::fs::metadata(path)
            .and_then(|m| m.modified())
            .map(|t| t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64)
            .unwrap_or(0);

        let project_path = cwd.unwrap_or_default();

        Some(DiscoveredClaudeSession {
            claude_session_id: claude_session_id.to_string(),
            project_path,
            first_user_message,
            last_assistant_text,
            file_modified,
            first_timestamp,
        })
    }



    /// Directly scan Claude CLI JSONL sessions, returning data straight from the source.
    /// No aginx metadata layer involved. sessionId IS the Claude session ID.
    pub fn scan_claude_sessions_direct(&self) -> Vec<serde_json::Value> {
        let claude_projects_dir = match dirs::home_dir() {
            Some(h) => h.join(".claude").join("projects"),
            None => return Vec::new(),
        };

        if !claude_projects_dir.exists() {
            return Vec::new();
        }

        let mut sessions: Vec<serde_json::Value> = Vec::new();

        if let Ok(entries) = std::fs::read_dir(&claude_projects_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    self.scan_project_dir_direct(&path, &mut sessions);
                }
            }
        }

        // Sort by updatedAt descending
        sessions.sort_by(|a, b| {
            b.get("updatedAt").and_then(|v| v.as_u64()).unwrap_or(0)
                .cmp(&a.get("updatedAt").and_then(|v| v.as_u64()).unwrap_or(0))
        });

        tracing::info!("Direct scanned {} Claude CLI sessions", sessions.len());
        sessions
    }

    /// Scan a single project directory for direct listing
    fn scan_project_dir_direct(
        &self,
        dir: &std::path::Path,
        results: &mut Vec<serde_json::Value>,
    ) {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }
                // Skip subagent files
                if path.to_str().map(|s| s.contains("/subagents/")).unwrap_or(false) {
                    continue;
                }

                let claude_session_id = match path.file_stem().and_then(|s| s.to_str()) {
                    Some(id) => id.to_string(),
                    None => continue,
                };

                if let Some(session) = self.parse_jsonl_metadata(&path, &claude_session_id) {
                    let file_modified = std::fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .map(|t| t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64)
                        .unwrap_or(0);

                    let title = session.first_user_message.clone().or_else(|| {
                        session.project_path.split('/').last().map(String::from)
                    });

                    results.push(serde_json::json!({
                        "sessionId": claude_session_id,
                        "agentId": "claude",
                        "title": title,
                        "lastMessage": session.last_assistant_text,
                        "workdir": session.project_path,
                        "createdAt": session.first_timestamp.unwrap_or(file_modified),
                        "updatedAt": file_modified
                    }));
                }
            }
        }
    }

    /// Read messages from a Claude JSONL file, with limit and system message filtering.
    /// Returns the last `limit` real conversation messages.
    pub fn read_claude_jsonl_messages_limited(
        &self,
        claude_session_id: &str,
        limit: usize,
    ) -> Vec<serde_json::Value> {
        let claude_projects_dir = match dirs::home_dir() {
            Some(h) => h.join(".claude").join("projects"),
            None => return Vec::new(),
        };

        let path = match self.find_claude_jsonl_path(claude_session_id, &claude_projects_dir) {
            Some(p) => p,
            None => {
                tracing::warn!("Claude JSONL not found for session {}", claude_session_id);
                return Vec::new();
            }
        };

        let file = match std::fs::File::open(&path) {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!("Failed to read Claude JSONL: {}", e);
                return Vec::new();
            }
        };

        let reader = std::io::BufReader::new(file);
        let mut all_messages: Vec<serde_json::Value> = Vec::new();
        let mut pending_tools: std::collections::HashMap<String, (String, String)> = std::collections::HashMap::new();

        for line in std::io::BufRead::lines(reader) {
            let line = match line {
                Ok(l) => l,
                Err(_) => break,
            };
            let line = line.trim();
            if line.is_empty() { continue; }

            if let Ok(event) = serde_json::from_str::<serde_json::Value>(line) {
                let event_type = event.get("type").and_then(|t| t.as_str());

                match event_type {
                    Some("user") => {
                        if let Some(msg) = event.get("message") {
                            let content_val = msg.get("content");
                            if let Some(text) = content_val.and_then(|c| c.as_str()) {
                                if !is_system_message(text) {
                                    all_messages.push(serde_json::json!({
                                        "role": "user",
                                        "content": text
                                    }));
                                }
                            } else if let Some(blocks) = content_val.and_then(|c| c.as_array()) {
                                for block in blocks {
                                    match block.get("type").and_then(|t| t.as_str()) {
                                        Some("tool_result") => {
                                            let tool_use_id = block.get("tool_use_id")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("");
                                            let result_content = block.get("content")
                                                .and_then(|c| c.as_str())
                                                .unwrap_or("");
                                            let tool_label = pending_tools.remove(tool_use_id)
                                                .map(|(name, args)| format!("{}({})", name, args))
                                                .unwrap_or_else(|| "tool".to_string());
                                            let display = if result_content.len() > 2000 {
                                                let cut = result_content.char_indices()
                                                    .take_while(|(i, _)| *i < 2000)
                                                    .last()
                                                    .map(|(i, c)| i + c.len_utf8())
                                                    .unwrap_or(0);
                                                format!("> **{}**\n\n{}...\n\n*(result truncated)*", tool_label, &result_content[..cut])
                                            } else if result_content.is_empty() {
                                                format!("> **{}**\n\n*(empty result)*", tool_label)
                                            } else {
                                                format!("> **{}**\n\n{}", tool_label, result_content)
                                            };
                                            all_messages.push(serde_json::json!({
                                                "role": "tool",
                                                "content": display
                                            }));
                                        }
                                        Some("text") => {
                                            if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                                                if !text.is_empty() && !is_system_message(text) {
                                                    all_messages.push(serde_json::json!({
                                                        "role": "user",
                                                        "content": text
                                                    }));
                                                }
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }
                    Some("assistant") => {
                        if let Some(msg) = event.get("message") {
                            if let Some(content_blocks) = msg.get("content").and_then(|c| c.as_array()) {
                                for block in content_blocks {
                                    match block.get("type").and_then(|t| t.as_str()) {
                                        Some("text") => {
                                            if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                                                if !text.is_empty() {
                                                    all_messages.push(serde_json::json!({
                                                        "role": "assistant",
                                                        "content": text
                                                    }));
                                                }
                                            }
                                        }
                                        Some("tool_use") => {
                                            let tool_id = block.get("id")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("").to_string();
                                            let tool_name = block.get("name")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("unknown").to_string();
                                            let input = block.get("input")
                                                .cloned()
                                                .unwrap_or(serde_json::json!({}));
                                            let key_args = crate::acp::agent_event::format_tool_title(&tool_name, &Some(input.clone()));
                                            pending_tools.insert(tool_id, (tool_name, key_args));
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        // Return last N messages
        let start = if all_messages.len() > limit { all_messages.len() - limit } else { 0 };
        all_messages[start..].to_vec()
    }

    /// Delete a Claude JSONL file by its session ID
    pub fn delete_claude_jsonl_by_session_id(&self, claude_session_id: &str) -> bool {
        let claude_projects_dir = match dirs::home_dir() {
            Some(h) => h.join(".claude").join("projects"),
            None => return false,
        };

        if let Some(path) = self.find_claude_jsonl_path(claude_session_id, &claude_projects_dir) {
            match std::fs::remove_file(&path) {
                Ok(()) => {
                    tracing::info!("Deleted Claude JSONL: {}", path.display());
                    true
                }
                Err(e) => {
                    tracing::warn!("Failed to delete Claude JSONL {}: {}", path.display(), e);
                    false
                }
            }
        } else {
            tracing::warn!("Claude JSONL not found for deletion: {}", claude_session_id);
            false
        }
    }
}

/// Discovered Claude CLI session (temporary struct for scanning)
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct DiscoveredClaudeSession {
    claude_session_id: String,
    project_path: String,
    first_user_message: Option<String>,
    last_assistant_text: Option<String>,
    file_modified: u64,
    first_timestamp: Option<u64>,
}


/// Parse ISO 8601 timestamp to unix millis
/// e.g., "2026-03-26T04:37:44.598Z"
fn parse_iso_timestamp(ts: &str) -> Option<u64> {
    // Simple parsing: extract date/time components
    let ts = ts.trim_end_matches('Z').trim_end_matches('z');
    let parts: Vec<&str> = ts.split(|c: char| c == 'T' || c == ':' || c == '.' || c == '-').collect();
    if parts.len() < 6 {
        return None;
    }

    let year: i32 = parts[0].parse().ok()?;
    let month: u32 = parts[1].parse().ok()?;
    let day: u32 = parts[2].parse().ok()?;
    let hour: u32 = parts[3].parse().ok()?;
    let minute: u32 = parts[4].parse().ok()?;
    let second: u32 = parts[5].parse().unwrap_or(0);

    // Calculate unix timestamp manually (days from 1970-01-01)
    let days = days_from_epoch(year, month, day)?;
    let secs = (days as u64 * 86400) + (hour as u64 * 3600) + (minute as u64 * 60) + (second as u64);
    Some(secs * 1000)
}

/// Calculate days since 1970-01-01
fn days_from_epoch(year: i32, month: u32, day: u32) -> Option<u64> {
    if month < 1 || month > 12 || day < 1 || day > 31 {
        return None;
    }

    let mut days: i64 = 0;
    // Years
    for y in 1970..year {
        days += if is_leap_year(y) { 366 } else { 365 };
    }
    // Months
    let days_in_months = [0, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    for m in 1..month {
        days += days_in_months[m as usize] as i64;
        if m == 2 && is_leap_year(year) {
            days += 1;
        }
    }
    // Days
    days += day as i64;

    if days < 0 { return None; }
    Some(days as u64)
}

fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// Format tool arguments into a short, human-readable summary.
///
/// Shows only the most relevant argument(s) for each tool type:

/// Check if a message is a system/meta message that should be hidden from display
fn is_system_message(text: &str) -> bool {
    text.contains("<local-command-caveat>")
        || text.contains("<command-name>")
        || text.contains("<local-command-stdout>")
        || text.contains("<local-command-stderr>")
        || text.contains("This session is being continued from a previous conversation")
}
