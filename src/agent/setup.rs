//! 首次启动引导 — 自动检测并配置常用 Agent
//!
//! 当 ~/.aginx/agents/ 为空时，交互式引导用户选择 Agent 模板，
//! 自动生成 aginx.toml 配置文件。

use std::io::{self, Write};

/// Agent 模板定义
struct AgentTemplate {
    id: &'static str,
    name: &'static str,
    description: &'static str,
    /// 检测命令，如 "which claude"
    detect_command: &'static str,
    /// 完整的 aginx.toml 内容
    config_toml: &'static str,
}

/// Claude Code 模板
const CLAUDE_TEMPLATE: AgentTemplate = AgentTemplate {
    id: "claude",
    name: "Claude",
    description: "Anthropic's coding assistant",
    detect_command: "which claude",
    config_toml: r#"id = "claude"
name = "Claude"
agent_type = "claude"
version = "1.0.0"
description = "Claude Code - Anthropic's coding assistant"

[command]
path = "claude"
args = ["--dangerously-skip-permissions", "--print", "--output-format", "stream-json", "--input-format", "stream-json", "--include-partial-messages", "--verbose"]

[session]
require_workdir = true
storage_path = "~/.claude/projects"

[session.resume]
session_id_path = "session_id"
resume_args = ["--resume", "${SESSION_ID}"]

[capabilities]
chat = true
code = true
streaming = true
permissions = true
"#,
};

/// 所有可用模板
const TEMPLATES: &[AgentTemplate] = &[CLAUDE_TEMPLATE];

/// 检查是否需要首次引导
pub fn needs_setup() -> bool {
    let agents_dir = crate::config::agents_dir();

    if !agents_dir.exists() {
        return true;
    }

    // 检查目录下是否有 aginx.toml
    let Ok(entries) = std::fs::read_dir(&agents_dir) else {
        return true;
    };

    for entry in entries.flatten() {
        let config_path = entry.path().join("aginx.toml");
        if config_path.exists() {
            return false;
        }
    }

    true
}

/// 检测模板对应的 CLI 是否已安装
fn detect_available(template: &AgentTemplate) -> bool {
    // detect_command 格式: "which claude"
    let parts: Vec<&str> = template.detect_command.split_whitespace().collect();
    if parts.len() < 2 {
        return false;
    }

    std::process::Command::new(parts[0])
        .args(&parts[1..])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// 运行首次引导
pub fn run_setup() -> anyhow::Result<()> {
    println!();
    println!("未检测到已配置的 Agent，开始初始化设置...");
    println!();

    // 检测可用性
    let available: Vec<(usize, bool)> = TEMPLATES
        .iter()
        .enumerate()
        .map(|(i, t)| (i, detect_available(t)))
        .collect();

    // 显示列表
    println!("可用 Agent:");
    for (i, template) in TEMPLATES.iter().enumerate() {
        let status = if available[i].1 {
            "✓ 已安装"
        } else {
            "✗ 未检测到"
        };
        println!("  {}. {} — {} ({})", i + 1, template.name, template.description, status);
    }
    println!();

    // 读取用户选择
    print!("请选择要配置的 Agent (输入编号，跳过输入 0) [1]: ");
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;

    let input = input.trim();
    let index: usize = if input.is_empty() {
        0 // 默认选第一个
    } else {
        match input.parse::<usize>() {
            Ok(n) if n == 0 => return Ok(()), // 跳过
            Ok(n) => n - 1,                   // 转为 0-indexed
            Err(_) => {
                println!("无效输入，跳过配置");
                return Ok(());
            }
        }
    };

    if index >= TEMPLATES.len() {
        println!("无效选择，跳过配置");
        return Ok(());
    }

    let template = &TEMPLATES[index];

    // 检查是否可用
    if !detect_available(template) {
        println!();
        println!("未检测到 {} CLI，请先安装后再试。", template.name);
        println!("跳过配置，你可以稍后手动创建 ~/.aginx/agents/{}/aginx.toml", template.id);
        return Ok(());
    }

    // 创建配置
    println!();
    println!("正在配置 {}...", template.name);

    let agents_dir = crate::config::agents_dir();
    let agent_dir = agents_dir.join(template.id);
    std::fs::create_dir_all(&agent_dir)?;

    let config_path = agent_dir.join("aginx.toml");
    std::fs::write(&config_path, template.config_toml)?;

    println!("  ✓ 已创建 {}", config_path.display());
    println!();
    println!("Agent 配置完成!");

    Ok(())
}
