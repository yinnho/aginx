//! Permission handling for aginx
//!
//! 处理 CLI 工具（如 Claude Code）的权限请求

use std::sync::Arc;
use tokio::sync::{Mutex, oneshot};
use uuid::Uuid;

/// 权限请求
#[derive(Debug, Clone)]
pub struct PermissionRequest {
    /// 请求 ID
    pub id: String,
    /// 会话 ID
    pub session_id: String,
    /// 权限描述（显示给用户的内容）
    pub description: String,
    /// 选项列表
    pub options: Vec<PermissionOption>,
}

/// 权限选项
#[derive(Debug, Clone)]
pub struct PermissionOption {
    /// 选项索引（从 1 开始）
    pub index: usize,
    /// 选项文本
    pub label: String,
    /// 是否是默认选项
    pub is_default: bool,
}

/// 权限响应
#[derive(Debug, Clone)]
pub struct PermissionResponse {
    /// 请求 ID
    pub request_id: String,
    /// 用户选择的选项索引
    pub choice: usize,
}

/// 权限请求管理器
pub struct PermissionManager {
    /// 等待中的权限请求（request_id -> (request, response_sender)）
    pending: Mutex<Vec<(PermissionRequest, oneshot::Sender<PermissionResponse>)>>,
}

impl PermissionManager {
    /// 创建新的权限管理器
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(Vec::new()),
        }
    }

    /// 解析 CLI 输出中的权限请求
    pub fn parse_permission_request(output: &str) -> Option<PermissionRequestPreview> {
        // 检测典型的权限请求模式
        // 例如：
        // Do you want to proceed?
        // ❯ 1. Yes
        //   2. Yes, and don't ask again for this session
        //   3. No

        let lines: Vec<&str> = output.lines().collect();

        // 查找 "Do you want" 或类似的提示
        let mut description_lines = Vec::new();
        let mut options = Vec::new();
        let mut found_prompt = false;
        let mut in_options = false;

        for line in &lines {
            let trimmed = line.trim();

            // 检测权限提示
            if !found_prompt {
                if trimmed.contains("Do you want") ||
                   trimmed.contains("Allow") ||
                   trimmed.contains("Proceed?") ||
                   trimmed.contains("Continue?") ||
                   trimmed.contains("是否允许") ||
                   trimmed.contains("请确认") {
                    found_prompt = true;
                    description_lines.push(trimmed.to_string());
                    continue;
                }
            }

            if found_prompt {
                // 检测选项行 (❯ 1. xxx 或 1. xxx 或 ● 1. xxx)
                if let Some(opt) = parse_option_line(trimmed) {
                    in_options = true;
                    options.push(opt);
                } else if in_options && trimmed.is_empty() {
                    // 选项结束
                    break;
                } else if !in_options && !trimmed.is_empty() {
                    // 描述继续
                    description_lines.push(trimmed.to_string());
                }
            }
        }

        if found_prompt && !options.is_empty() {
            Some(PermissionRequestPreview {
                description: description_lines.join("\n"),
                options,
            })
        } else {
            None
        }
    }

    /// 创建新的权限请求并等待响应
    pub async fn request_permission(
        &self,
        session_id: &str,
        preview: PermissionRequestPreview,
    ) -> PermissionResponse {
        let request_id = format!("perm_{}", Uuid::new_v4().simple());
        let (tx, rx) = oneshot::channel();

        let request = PermissionRequest {
            id: request_id.clone(),
            session_id: session_id.to_string(),
            description: preview.description,
            options: preview.options,
        };

        tracing::info!("权限请求 [{}]: {}", request_id, request.description);

        // 添加到等待队列
        {
            let mut pending = self.pending.lock().await;
            pending.push((request, tx));
        }

        // 等待响应
        match rx.await {
            Ok(response) => {
                tracing::info!("权限响应 [{}]: choice={}", request_id, response.choice);
                response
            }
            Err(_) => {
                tracing::warn!("权限请求 [{}] 被取消", request_id);
                PermissionResponse {
                    request_id,
                    choice: 0, // 取消
                }
            }
        }
    }

    /// 获取等待中的权限请求
    pub async fn get_pending_request(&self, session_id: &str) -> Option<PermissionRequest> {
        let pending = self.pending.lock().await;
        pending
            .iter()
            .find(|(req, _)| req.session_id == session_id)
            .map(|(req, _)| req.clone())
    }

    /// 响应权限请求
    pub async fn respond(&self, request_id: &str, choice: usize) -> bool {
        let mut pending = self.pending.lock().await;

        if let Some(idx) = pending.iter().position(|(req, _)| req.id == request_id) {
            let (_, tx) = pending.remove(idx);
            let response = PermissionResponse {
                request_id: request_id.to_string(),
                choice,
            };
            tx.send(response).is_ok()
        } else {
            tracing::warn!("权限请求 [{}] 不存在", request_id);
            false
        }
    }

    /// 取消会话的所有权限请求
    pub async fn cancel_session(&self, session_id: &str) {
        let mut pending = self.pending.lock().await;
        pending.retain(|(req, _)| req.session_id != session_id);
    }
}

impl Default for PermissionManager {
    fn default() -> Self {
        Self::new()
    }
}

/// 权限请求预览（解析后的原始数据）
#[derive(Debug, Clone)]
pub struct PermissionRequestPreview {
    pub description: String,
    pub options: Vec<PermissionOption>,
}

/// 解析选项行
fn parse_option_line(line: &str) -> Option<PermissionOption> {
    // 移除前导符号 (❯, ●, ○, 等)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_permission_request() {
        let output = r#"Do you want to proceed?
❯ 1. Yes
  2. Yes, and don't ask again for this session
  3. No"#;

        let result = PermissionManager::parse_permission_request(output);
        assert!(result.is_some());

        let preview = result.unwrap();
        assert_eq!(preview.description, "Do you want to proceed?");
        assert_eq!(preview.options.len(), 3);
        assert_eq!(preview.options[0].label, "Yes");
        assert_eq!(preview.options[1].label, "Yes, and don't ask again for this session");
        assert_eq!(preview.options[2].label, "No");
    }
}
