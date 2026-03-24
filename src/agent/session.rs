//! Session management for aginx
//!
//! 会话 = 一个长期运行的 agent 进程 (Process 类型)
//! 或基于工作目录的会话 (Claude 类型)

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};
use uuid::Uuid;

use super::manager::AgentInfo;
use crate::config::AgentType;

/// 权限提示
#[derive(Debug, Clone, serde::Serialize)]
pub struct PermissionPrompt {
    /// 请求 ID
    pub request_id: String,
    /// 描述文本
    pub description: String,
    /// 选项列表
    pub options: Vec<PermissionOption>,
}

/// 权限选项
#[derive(Debug, Clone, serde::Serialize)]
pub struct PermissionOption {
    pub index: usize,
    pub label: String,
    #[serde(rename = "isDefault")]
    pub is_default: bool,
}

/// 发送消息的结果
#[derive(Debug, Clone, serde::Serialize)]
pub enum SendMessageResult {
    /// 正常响应
    Response(String),
    /// 需要权限确认
    PermissionNeeded(PermissionPrompt),
    /// 权限已处理，继续执行
    PermissionHandled {
        /// 原始请求 ID
        request_id: String,
        /// 最终响应
        response: String,
    },
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
    /// Claude session UUID (用于 --session-id 参数)
    pub claude_session_uuid: Option<String>,
}

/// 解析权限提示
fn parse_permission_prompt(output: &str) -> Option<PermissionPrompt> {
    let lines: Vec<&str> = output.lines().collect();
    let mut description_lines = Vec::new();
    let mut options = Vec::new();
    let mut found_prompt = false;
    let mut in_options = false;

    for line in &lines {
        let trimmed = line.trim();

        // 检测权限提示关键词
        if !found_prompt {
            if trimmed.contains("Do you want") ||
               trimmed.contains("Allow") ||
               trimmed.contains("Proceed?") ||
               trimmed.contains("Continue?") ||
               trimmed.starts_with("❯") && trimmed.contains("1.") {
                found_prompt = true;
                // 如果行以 ❯ 开头，说明是选项行
                if trimmed.starts_with("❯") {
                    if let Some(opt) = parse_option_line(trimmed) {
                        options.push(opt);
                        in_options = true;
                    }
                } else {
                    description_lines.push(trimmed.to_string());
                }
                continue;
            }
        }

        if found_prompt {
            // 检测选项行
            if let Some(opt) = parse_option_line(trimmed) {
                in_options = true;
                options.push(opt);
            } else if in_options && trimmed.is_empty() {
                // 选项结束
                break;
            } else if !in_options && !trimmed.is_empty() && !trimmed.starts_with("❯") {
                // 描述继续
                description_lines.push(trimmed.to_string());
            } else if trimmed.starts_with("❯") || trimmed.starts_with("  ") {
                // 可能是选项行
                if let Some(opt) = parse_option_line(trimmed) {
                    options.push(opt);
                    in_options = true;
                }
            }
        }
    }

    if found_prompt && !options.is_empty() {
        Some(PermissionPrompt {
            request_id: format!("perm_{}", Uuid::new_v4().simple()),
            description: if description_lines.is_empty() {
                "Permission required".to_string()
            } else {
                description_lines.join("\n")
            },
            options,
        })
    } else {
        None
    }
}

/// 解析选项行
fn parse_option_line(line: &str) -> Option<PermissionOption> {
    // 移除前导符号 (❯, ●, ○, 空格)
    let line = line.trim_start_matches(['❯', '●', '○', ' ']).trim();

    // 匹配 "1. xxx" 或 "1) xxx" 格式
    let re = regex::Regex::new(r"^(\d+)[.\)]\s*(.+)$").ok()?;

    if let Some(caps) = re.captures(line) {
        let index: usize = caps.get(1)?.as_str().parse().ok()?;
        let label = caps.get(2)?.as_str().trim().to_string();
        let is_default = line.starts_with('❯') || line.starts_with('●');

        Some(PermissionOption {
            index,
            label,
            is_default,
        })
    } else {
        None
    }
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

/// 待处理的权限请求
#[derive(Debug, Clone)]
struct PendingPermission {
    /// 请求 ID
    request_id: String,
    /// 原始消息
    original_message: String,
    /// 权限提示
    prompt: PermissionPrompt,
}

/// 会话状态
pub struct Session {
    /// 会话 ID
    pub id: String,
    /// Agent ID
    pub agent_id: String,
    /// Agent 类型
    agent_type: AgentType,
    /// 工作目录
    workdir: Option<String>,
    /// 进程 stdin 写入端 (仅用于 Process 类型)
    stdin: Mutex<Option<std::process::ChildStdin>>,
    /// 进程 stdout 读取端 (仅用于 Process 类型)
    stdout: Mutex<Option<BufReader<std::process::ChildStdout>>>,
    /// 最后活动时间
    pub last_activity: Mutex<Instant>,
    /// 进程句柄（用于关闭时 kill, 仅用于 Process 类型）
    process: Mutex<Option<std::process::Child>>,
    /// Claude 会话 UUID (仅用于 Claude 类型，用于 --session-id 参数)
    claude_session_uuid: Option<String>,
    /// 信号量许可（持有它来限制并发）
    #[allow(dead_code)]
    permit: OwnedSemaphorePermit,
    /// 待处理的权限请求 (仅用于 Claude 类型)
    pending_permission: Mutex<Option<PendingPermission>>,
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
    pub fn new(agent_info: &AgentInfo, workdir: Option<&str>, permit: OwnedSemaphorePermit) -> Result<Self, String> {
        let session_id = format!("sess_{}", Uuid::new_v4().simple());

        match agent_info.agent_type {
            AgentType::Claude => {
                // Claude 类型：生成专用的 Claude session UUID，用于 --session-id 参数
                let claude_uuid = Uuid::new_v4().to_string();
                tracing::info!("会话 [{}] 创建成功 (Claude 模式)，Agent: {}, Claude UUID: {}", session_id, agent_info.id, claude_uuid);
                Ok(Self {
                    id: session_id,
                    agent_id: agent_info.id.clone(),
                    agent_type: AgentType::Claude,
                    workdir: workdir.map(|s| s.to_string()),
                    stdin: Mutex::new(None),
                    stdout: Mutex::new(None),
                    last_activity: Mutex::new(Instant::now()),
                    process: Mutex::new(None),
                    claude_session_uuid: Some(claude_uuid),
                    permit,
                    pending_permission: Mutex::new(None),
                })
            }
            AgentType::Process => {
                // Process 类型：启动持久进程
                let (stdin, stdout, process) = Self::spawn_process(agent_info, workdir)?;
                tracing::info!("会话 [{}] 创建成功 (Process 模式)，Agent: {}", session_id, agent_info.id);
                Ok(Self {
                    id: session_id,
                    agent_id: agent_info.id.clone(),
                    agent_type: AgentType::Process,
                    workdir: workdir.map(|s| s.to_string()),
                    stdin: Mutex::new(Some(stdin)),
                    stdout: Mutex::new(Some(stdout)),
                    last_activity: Mutex::new(Instant::now()),
                    process: Mutex::new(Some(process)),
                    claude_session_uuid: None,
                    permit,
                    pending_permission: Mutex::new(None),
                })
            }
            AgentType::Builtin => {
                Err("Builtin agents do not support sessions".to_string())
            }
        }
    }

    /// 启动外部进程
    fn spawn_process(agent_info: &AgentInfo, workdir: Option<&str>) -> Result<
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
            .stderr(Stdio::null()); // 忽略 stderr

        // 添加参数
        for arg in &agent_info.args {
            cmd.arg(arg);
        }

        // 设置工作目录 (优先使用传入的 workdir，其次使用配置的 working_dir)
        if let Some(dir) = workdir {
            cmd.current_dir(dir);
        } else if let Some(ref dir) = agent_info.working_dir {
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
    pub async fn send_message(&self, message: &str) -> Result<SendMessageResult, String> {
        // 更新活动时间
        {
            let mut last = self.last_activity.lock().await;
            *last = Instant::now();
        }

        match self.agent_type {
            AgentType::Claude => {
                // Claude 类型：每次消息启动新进程，使用 -c 保持上下文
                self.send_message_claude(message).await
            }
            AgentType::Process => {
                // Process 类型：使用持久进程
                self.send_message_process(message).await.map(|r| SendMessageResult::Response(r))
            }
            AgentType::Builtin => {
                Err("Builtin agents do not support sessions".to_string())
            }
        }
    }

    /// Claude 类型：发送消息（每次启动新进程，使用 --session-id 保持上下文）
    async fn send_message_claude(&self, message: &str) -> Result<SendMessageResult, String> {
        use tokio::process::Command as AsyncCommand;

        // 获取 Claude session UUID
        let claude_uuid = self.claude_session_uuid.as_ref()
            .ok_or("Claude session UUID not set")?;

        // 尝试找到 claude 命令的完整路径
        let claude_path = if let Ok(path) = std::env::var("HOME") {
            let npm_path = format!("{}/.npm-global/bin/claude", path);
            if std::path::Path::new(&npm_path).exists() {
                npm_path
            } else {
                "claude".to_string()
            }
        } else {
            "claude".to_string()
        };

        tracing::debug!("Claude 会话 [{}] 发送消息到 {}, UUID: {}", self.id, claude_path, claude_uuid);

        let mut cmd = AsyncCommand::new(&claude_path);
        cmd.arg("--print")
            .arg("--session-id")
            .arg(claude_uuid)
            .env_remove("CLAUDECODE");

        if let Some(ref dir) = self.workdir {
            cmd.current_dir(dir);
        }

        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd.spawn()
            .map_err(|e| format!("Failed to spawn claude: {}", e))?;

        // 写入消息到 stdin
        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            stdin.write_all(message.as_bytes())
                .await
                .map_err(|e| format!("Write error: {}", e))?;
            stdin.flush()
                .await
                .map_err(|e| format!("Flush error: {}", e))?;
        }

        // 读取响应
        let output = child.wait_with_output()
            .await
            .map_err(|e| format!("Wait error: {}", e))?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let combined = format!("{}{}", stdout, stderr);

        // 检测权限提示
        if let Some(perm) = parse_permission_prompt(&combined) {
            // 存储待处理的权限请求
            let pending = PendingPermission {
                request_id: perm.request_id.clone(),
                original_message: message.to_string(),
                prompt: perm.clone(),
            };
            {
                let mut pending_perm = self.pending_permission.lock().await;
                *pending_perm = Some(pending);
            }
            return Ok(SendMessageResult::PermissionNeeded(perm));
        }

        if output.status.success() {
            Ok(SendMessageResult::Response(stdout.trim().to_string()))
        } else {
            Err(format!("Claude error: {}", stderr))
        }
    }

    /// Process 类型：发送消息（使用持久进程）
    async fn send_message_process(&self, message: &str) -> Result<String, String> {
        // 检查进程是否仍然存活
        {
            let mut process = self.process.lock().await;
            if let Some(ref mut child) = *process {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        tracing::warn!("进程已退出，状态: {}", status);
                        return Err("Process has exited".to_string());
                    }
                    Ok(None) => {
                        // 进程仍在运行
                    }
                    Err(e) => {
                        tracing::error!("检查进程状态失败: {}", e);
                    }
                }
            }
        }

        // 写入消息
        {
            let mut stdin_opt = self.stdin.lock().await;
            if let Some(ref mut stdin) = *stdin_opt {
                writeln!(stdin, "{}", message).map_err(|e| format!("Write error: {}", e))?;
                stdin.flush().map_err(|e| format!("Flush error: {}", e))?;
            } else {
                return Err("No stdin available".to_string());
            }
        }

        // 读取响应
        let mut response = String::new();
        {
            let mut stdout_opt = self.stdout.lock().await;
            if let Some(ref mut stdout) = *stdout_opt {
                match stdout.read_line(&mut response) {
                    Ok(0) => return Err("Process closed".to_string()),
                    Ok(_) => {}
                    Err(e) => return Err(format!("Read error: {}", e)),
                }
            } else {
                return Err("No stdout available".to_string());
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

        Ok(session_id)
    }

    /// 向会话发送消息
    pub async fn send_message(&self, session_id: &str, message: &str) -> Result<SendMessageResult, String> {
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

    /// 获取会话信息 (用于流式输出)
    pub async fn get_session_info(&self, session_id: &str) -> Option<SessionInfo> {
        let sessions = self.sessions.lock().await;
        sessions.get(session_id).map(|s| SessionInfo {
            session_id: s.id.clone(),
            agent_id: s.agent_id.clone(),
            workdir: s.workdir.clone(),
            claude_session_uuid: s.claude_session_uuid.clone(),
        })
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

    /// 响应权限请求
    pub async fn respond_permission(&self, session_id: &str, choice: usize) -> Result<SendMessageResult, String> {
        use tokio::process::Command as AsyncCommand;

        // 获取 session
        let sessions = self.sessions.lock().await;
        let session = sessions.get(session_id)
            .ok_or_else(|| format!("Session {} not found", session_id))?;

        // 获取待处理的权限请求
        let pending = {
            let mut pending_perm = session.pending_permission.lock().await;
            pending_perm.take()
        };

        let pending = pending.ok_or("No pending permission request")?;
        tracing::info!("响应权限请求 [{}]: choice={}", pending.request_id, choice);

        // 获取 Claude session UUID
        let claude_uuid = session.claude_session_uuid.as_ref()
            .ok_or("Claude session UUID not set")?;

        // 尝试找到 claude 命令的完整路径
        let claude_path = if let Ok(path) = std::env::var("HOME") {
            let npm_path = format!("{}/.npm-global/bin/claude", path);
            if std::path::Path::new(&npm_path).exists() {
                npm_path
            } else {
                "claude".to_string()
            }
        } else {
            "claude".to_string()
        };

        // 构建命令，使用 --print 和 --session-id 保持上下文
        let mut cmd = AsyncCommand::new(&claude_path);
        cmd.arg("--print")
            .arg("--session-id")
            .arg(claude_uuid)
            .env_remove("CLAUDECODE");

        if let Some(ref dir) = session.workdir {
            cmd.current_dir(dir);
        }

        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd.spawn()
            .map_err(|e| format!("Failed to spawn claude: {}", e))?;

        // 写入权限选择到 stdin
        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            let choice_input = format!("{}\n", choice);
            stdin.write_all(choice_input.as_bytes())
                .await
                .map_err(|e| format!("Write error: {}", e))?;
            stdin.flush()
                .await
                .map_err(|e| format!("Flush error: {}", e))?;
        }

        // 读取响应
        let output = child.wait_with_output()
            .await
            .map_err(|e| format!("Wait error: {}", e))?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let combined = format!("{}{}", stdout, stderr);

        // 检测是否还有权限提示
        if let Some(perm) = parse_permission_prompt(&combined) {
            // 存储新的待处理权限请求
            let new_pending = PendingPermission {
                request_id: perm.request_id.clone(),
                original_message: pending.original_message.clone(),
                prompt: perm.clone(),
            };
            {
                let mut pending_perm = session.pending_permission.lock().await;
                *pending_perm = Some(new_pending);
            }
            return Ok(SendMessageResult::PermissionNeeded(perm));
        }

        if output.status.success() {
            Ok(SendMessageResult::Response(stdout.trim().to_string()))
        } else {
            Err(format!("Claude error: {}", stderr))
        }
    }
}
