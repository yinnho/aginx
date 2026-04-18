//! Agent discovery - scan directories for aginx.toml config files

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use crate::config::AccessMode;

/// aginx.toml config structure
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentConfig {
    /// Agent ID (required)
    pub id: String,
    /// Display name
    pub name: String,
    /// Agent type (e.g. "claude", "copilot", "process")
    pub agent_type: String,
    /// Description
    #[serde(default)]
    pub description: String,
    /// Version
    #[serde(default)]
    pub version: String,
    /// Access mode: public | private (inherits global default if not set)
    #[serde(default)]
    pub access: Option<AccessMode>,

    /// Command config
    #[serde(default)]
    pub command: Option<CommandConfig>,

    /// Session config
    #[serde(default)]
    pub session: Option<SessionConfig>,

    /// Process timeout in seconds (default: 120)
    #[serde(default)]
    pub timeout: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CommandConfig {
    /// Command path (defaults to agent_type if not set)
    pub path: Option<String>,
    /// Command arguments
    #[serde(default)]
    pub args: Vec<String>,
    /// Environment variables
    #[serde(default)]
    pub env: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionConfig {
    /// Resume args template (e.g. ["--resume", "${SESSION_ID}"])
    #[serde(default)]
    pub resume_args: Vec<String>,
}

/// Discovered agent info
#[derive(Debug, Clone, Serialize)]
pub struct DiscoveredAgent {
    pub config_path: PathBuf,
    pub project_dir: PathBuf,
    pub config: AgentConfig,
    pub available: bool,
    pub error: Option<String>,
}

/// Scan directory for aginx.toml files
pub fn scan_directory(base_path: &Path, max_depth: usize) -> Vec<DiscoveredAgent> {
    let mut discovered = Vec::new();
    scan_recursive(base_path, base_path, max_depth, &mut discovered);
    discovered
}

fn scan_recursive(
    base_path: &Path,
    current_path: &Path,
    remaining_depth: usize,
    discovered: &mut Vec<DiscoveredAgent>,
) {
    if remaining_depth == 0 {
        return;
    }

    let config_path = current_path.join("aginx.toml");
    if config_path.exists() {
        match parse_aginx_toml(&config_path, current_path) {
            Ok(agent) => {
                if !discovered.iter().any(|a| a.config.id == agent.config.id) {
                    discovered.push(agent);
                }
            }
            Err(e) => {
                tracing::warn!("Failed to parse {}: {}", config_path.display(), e);
            }
        }
        return;
    }

    if let Ok(entries) = std::fs::read_dir(current_path) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if name.starts_with('.') || name == "node_modules" || name == "target" || name == "vendor" {
                        continue;
                    }
                }
                scan_recursive(base_path, &path, remaining_depth - 1, discovered);
            }
        }
    }
}

/// Parse aginx.toml file
pub fn parse_aginx_toml(path: &Path, project_dir: &Path) -> Result<DiscoveredAgent, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read file: {}", e))?;

    let config: AgentConfig = toml::from_str(&content)
        .map_err(|e| format!("Failed to parse TOML: {}", e))?;

    if config.id.is_empty() {
        return Err("Missing required field: id".to_string());
    }
    if config.name.is_empty() {
        return Err("Missing required field: name".to_string());
    }

    let (available, error) = check_available(&config);

    Ok(DiscoveredAgent {
        config_path: path.to_path_buf(),
        project_dir: project_dir.to_path_buf(),
        config,
        available,
        error,
    })
}

/// Check if agent CLI is available
fn check_available(config: &AgentConfig) -> (bool, Option<String>) {
    let cmd = config.command.as_ref().and_then(|c| c.path.as_deref())
        .unwrap_or_else(|| {
            if config.agent_type.is_empty() || config.agent_type == "process" {
                ""
            } else {
                config.agent_type.as_str()
            }
        });

    if cmd.is_empty() {
        return (false, Some("No command configured".to_string()));
    }

    if which::which(cmd).is_ok() || Path::new(cmd).exists() {
        (true, None)
    } else {
        (false, Some(format!("{} not found", cmd)))
    }
}

/// Convert AgentConfig to runtime AgentInfo
pub fn agent_config_to_info(config: AgentConfig, project_dir: &std::path::Path, global_access: &AccessMode) -> super::manager::AgentInfo {
    let access = config.access.unwrap_or(*global_access);

    let command = config.command
        .as_ref()
        .and_then(|c| c.path.clone())
        .unwrap_or_else(|| {
            if config.agent_type.is_empty() || config.agent_type == "process" {
                String::new()
            } else {
                config.agent_type.clone()
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

    let resume_args = config.session
        .as_ref()
        .map(|s| s.resume_args.clone())
        .filter(|a| !a.is_empty());

    super::manager::AgentInfo {
        id: config.id,
        name: config.name,
        description: config.description,
        agent_type: config.agent_type,
        command,
        args,
        env,
        timeout: config.timeout,
        resume_args,
        working_dir: Some(project_dir.to_string_lossy().to_string()),
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
name = "Claude"
agent_type = "claude"

[command]
path = "claude"
args = ["--print"]

[session]
resume_args = ["--resume", "${SESSION_ID}"]
"#;

        let config: AgentConfig = toml::from_str(toml_content).unwrap();
        assert_eq!(config.id, "claude");
        assert_eq!(config.name, "Claude");
        assert_eq!(config.command.unwrap().args, vec!["--print",]);
        let session = config.session.unwrap();
        assert_eq!(session.resume_args, vec!["--resume", "${SESSION_ID}"]);
    }
}
