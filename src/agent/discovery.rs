//! Agent discovery - 扫描目录查找 aginx.toml 配置文件

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use crate::config::AccessMode;

/// aginx.toml 配置文件结构
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentConfig {
    /// Agent ID（必需）
    pub id: String,
    /// 显示名称
    pub name: String,
    /// Agent 类型: claude, process, builtin
    pub agent_type: String,
    /// 版本
    #[serde(default)]
    pub version: String,
    /// 描述
    #[serde(default)]
    pub description: String,

    /// 访问模式: public | private，不设则继承全局默认
    #[serde(default)]
    pub access: Option<AccessMode>,

    /// 命令配置
    #[serde(default)]
    pub command: Option<CommandConfig>,

    /// 会话配置
    #[serde(default)]
    pub session: Option<SessionConfig>,

    /// 检测配置
    #[serde(default)]
    pub detect: Option<DetectConfig>,

    /// 能力声明
    #[serde(default)]
    pub capabilities: Option<CapabilitiesConfig>,


    /// 权限配置
    #[serde(default)]
    pub permissions: Option<PermissionsConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CommandConfig {
    /// 命令路径（可选，默认自动发现）
    pub path: Option<String>,
    /// 命令参数
    #[serde(default)]
    pub args: Vec<String>,
    /// 环境变量
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// 需要移除的环境变量
    #[serde(default)]
    pub env_remove: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionConfig {
    /// 是否需要工作目录
    #[serde(default)]
    pub require_workdir: bool,

    /// 会话恢复配置
    pub resume: Option<SessionResumeConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionResumeConfig {
    /// 从响应 JSON 中提取 session_id 的路径
    pub session_id_path: String,
    /// 恢复会话时的参数模板
    #[serde(default)]
    pub resume_args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DetectConfig {
    /// 检测命令
    pub check_command: Option<String>,
    /// 检测参数
    #[serde(default)]
    pub check_args: Vec<String>,
    /// 版本解析正则
    pub version_regex: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CapabilitiesConfig {
    #[serde(default)]
    pub chat: bool,
    #[serde(default)]
    pub code: bool,
    #[serde(default)]
    pub ask: bool,
    #[serde(default)]
    pub streaming: bool,
    #[serde(default)]
    pub permissions: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PermissionsConfig {
    /// 默认允许的工具列表
    #[serde(default)]
    pub default_allowed: Vec<String>,
}

/// 发现的 Agent 信息
#[derive(Debug, Clone, Serialize)]
pub struct DiscoveredAgent {
    /// 配置文件路径
    pub config_path: PathBuf,
    /// 项目根目录
    pub project_dir: PathBuf,
    /// 解析后的配置
    pub config: AgentConfig,
    /// 是否可用（检测命令是否成功）
    pub available: bool,
    /// 错误信息
    pub error: Option<String>,
}

/// 扫描目录查找 aginx.toml 文件
pub fn scan_directory(base_path: &Path, max_depth: usize) -> Vec<DiscoveredAgent> {
    let mut discovered = Vec::new();
    scan_directory_recursive(base_path, base_path, max_depth, &mut discovered);
    discovered
}

fn scan_directory_recursive(
    base_path: &Path,
    current_path: &Path,
    remaining_depth: usize,
    discovered: &mut Vec<DiscoveredAgent>,
) {
    if remaining_depth == 0 {
        return;
    }

    // 检查当前目录是否有 aginx.toml
    let config_path = current_path.join("aginx.toml");
    if config_path.exists() {
        match parse_aginx_toml(&config_path, current_path) {
            Ok(agent) => {
                // 检查是否已经发现过同名的
                if !discovered.iter().any(|a: &DiscoveredAgent| a.config.id == agent.config.id) {
                    discovered.push(agent);
                }
            }
            Err(e) => {
                tracing::warn!("Failed to parse {}: {}", config_path.display(), e);
            }
        }
        // 找到 aginx.toml 后，不再递归这个目录的子目录
        return;
    }

    // 递归扫描子目录
    if let Ok(entries) = std::fs::read_dir(current_path) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                // 跳过隐藏目录和常见的非项目目录
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if name.starts_with('.') || name == "node_modules" || name == "target" || name == "vendor" {
                        continue;
                    }
                }
                scan_directory_recursive(base_path, &path, remaining_depth - 1, discovered);
            }
        }
    }
}

/// 解析 aginx.toml 文件
pub fn parse_aginx_toml(path: &Path, project_dir: &Path) -> Result<DiscoveredAgent, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read file: {}", e))?;

    let config: AgentConfig = toml::from_str(&content)
        .map_err(|e| format!("Failed to parse TOML: {}", e))?;

    // 验证必需字段
    if config.id.is_empty() {
        return Err("Missing required field: id".to_string());
    }
    if config.name.is_empty() {
        return Err("Missing required field: name".to_string());
    }
    if config.agent_type.is_empty() {
        return Err("Missing required field: agent_type".to_string());
    }

    // 检测是否可用
    let (available, error) = check_agent_available(&config, project_dir);

    Ok(DiscoveredAgent {
        config_path: path.to_path_buf(),
        project_dir: project_dir.to_path_buf(),
        config,
        available,
        error,
    })
}

/// 检查 agent 是否可用
fn check_agent_available(config: &AgentConfig, project_dir: &Path) -> (bool, Option<String>) {
    // 如果有检测配置，运行检测命令
    if let Some(ref detect) = config.detect {
        if let Some(ref check_cmd) = detect.check_command {
            let mut cmd = std::process::Command::new(check_cmd);
            cmd.args(&detect.check_args);
            cmd.current_dir(project_dir);

            match cmd.output() {
                Ok(output) => {
                    if output.status.success() {
                        (true, None)
                    } else {
                        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                        (false, Some(format!("Check command failed: {}", stderr)))
                    }
                }
                Err(e) => {
                    // 命令不存在，但可能只是路径问题
                    if e.kind() == std::io::ErrorKind::NotFound {
                        // 检查是否在 PATH 中
                        if which::which(check_cmd).is_ok() {
                            (true, None)
                        } else {
                            (false, Some(format!("Command not found: {}", check_cmd)))
                        }
                    } else {
                        (false, Some(format!("Failed to run check: {}", e)))
                    }
                }
            }
        } else {
            (true, None)
        }
    } else {
        // 没有检测配置，根据 agent_type 检测默认命令
        match config.agent_type.as_str() {
            "claude" => {
                // 检查 claude 命令是否可用
                if which::which("claude").is_ok() {
                    (true, None)
                } else {
                    (false, Some("claude command not found in PATH".to_string()))
                }
            }
            "process" => {
                // 需要配置 command.path
                if let Some(ref cmd_config) = config.command {
                    if let Some(ref path) = cmd_config.path {
                        if Path::new(path).exists() || which::which(path).is_ok() {
                            (true, None)
                        } else {
                            (false, Some(format!("Command not found: {}", path)))
                        }
                    } else {
                        (false, Some("Missing command.path for process agent".to_string()))
                    }
                } else {
                    (false, Some("Missing command config for process agent".to_string()))
                }
            }
            "builtin" => (false, Some("builtin type is no longer supported, use process or claude".to_string())),
            _ => (true, None),
        }
    }
}

/// 将 AgentConfig 转换为 AgentInfo（用于注册和自动加载）
pub fn agent_config_to_info(config: AgentConfig, project_dir: &std::path::Path, global_access: &AccessMode) -> super::manager::AgentInfo {
    use crate::config::AgentType;

    let access = config.access.clone().unwrap_or_else(|| global_access.clone());

    let agent_type = match config.agent_type.as_str() {
        "claude" => AgentType::Claude,
        _ => AgentType::Process,
    };

    let command = config.command
        .as_ref()
        .and_then(|c| c.path.clone())
        .unwrap_or_else(|| {
            match agent_type {
                AgentType::Claude => "claude".to_string(),
                _ => String::new(),
            }
        });

    let args = config.command
        .as_ref()
        .map(|c| c.args.clone())
        .unwrap_or_default();

    let env = config.command
        .as_ref()
        .map(|c| c.env.clone())
        .unwrap_or_default();

    let env_remove = config.command
        .as_ref()
        .map(|c| c.env_remove.clone())
        .unwrap_or_default();

    let session_args = config.session
        .as_ref()
        .and_then(|s| s.resume.as_ref())
        .map(|r| r.resume_args.clone())
        .unwrap_or_default();

    let mut capabilities = Vec::new();
    if let Some(ref caps) = config.capabilities {
        if caps.chat { capabilities.push("chat".to_string()); }
        if caps.code { capabilities.push("code".to_string()); }
        if caps.ask { capabilities.push("ask".to_string()); }
        if caps.streaming { capabilities.push("streaming".to_string()); }
        if caps.permissions { capabilities.push("permissions".to_string()); }
    }
    if capabilities.is_empty() {
        capabilities = vec!["chat".to_string(), "code".to_string(), "ask".to_string()];
    }

    super::manager::AgentInfo {
        id: config.id,
        name: config.name,
        agent_type,
        command,
        args,
        env,
        env_remove,
        session_args,
        working_dir: Some(project_dir.to_string_lossy().to_string()),
        require_workdir: config.session.as_ref().map(|s| s.require_workdir).unwrap_or(false),
        capabilities,
        default_allowed_tools: config.permissions.as_ref()
            .map(|p| p.default_allowed.clone())
            .unwrap_or_default(),
        access,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_aginx_toml() {
        let toml_content = r#"
id = "claude"
name = "Claude Agent"
agent_type = "claude"
version = "1.0.0"

[command]
args = ["--print", "--output-format", "json"]

[session]
require_workdir = true

[session.resume]
session_id_path = "session_id"
resume_args = ["--resume", "${SESSION_ID}"]
"#;

        let config: AgentConfig = toml::from_str(toml_content).unwrap();
        assert_eq!(config.id, "claude");
        assert_eq!(config.name, "Claude Agent");
        assert_eq!(config.agent_type, "claude");
        assert!(config.session.unwrap().require_workdir);
    }
}
