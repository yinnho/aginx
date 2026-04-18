//! aginx - Agent Protocol 实现
//!
//! 让用户可以像访问网站一样访问 Agent
//!
//! Usage:
//!   aginx                              # 根据 config.toml 启动（默认 relay 模式，自动申请 ID）
//!   aginx --mode direct                # 直连模式，监听 TCP 86

mod config;
mod server;
mod agent;
mod relay;
mod binding;
mod acp;
mod auth;
mod fingerprint;

use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand, ValueEnum};
use crate::config::{Config, ServerMode, save_config, get_default_config_path};
use crate::agent::AgentManager;

/// aginx - Agent Protocol 实现
#[derive(Parser, Debug)]
#[command(name = "aginx")]
#[command(version, about, long_about = None)]
struct Args {
    #[command(subcommand)]
    command: Option<Commands>,

    /// 配置文件路径
    #[arg(short = 'c', long, value_name = "FILE")]
    config: Option<PathBuf>,

    /// 运行模式: direct(直连) | relay(中继，默认)
    #[arg(short = 'm', long, value_enum)]
    mode: Option<ModeArg>,

    /// 本地服务端口
    #[arg(short = 'p', long)]
    port: Option<u16>,

    /// 绑定地址
    #[arg(short = 'H', long)]
    host: Option<String>,

    /// 公网访问地址 (mode=direct 时使用)
    #[arg(long, value_name = "URL")]
    public_url: Option<String>,

    /// 启用详细日志
    #[arg(short = 'v', long)]
    verbose: bool,

    /// 启用调试日志
    #[arg(short = 'd', long)]
    debug: bool,
}

/// 命令行模式参数
#[derive(Debug, Clone, ValueEnum)]
enum ModeArg {
    Direct,
    Relay,
}

impl From<ModeArg> for ServerMode {
    fn from(mode: ModeArg) -> Self {
        match mode {
            ModeArg::Direct => ServerMode::Direct,
            ModeArg::Relay => ServerMode::Relay,
        }
    }
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// 生成配置文件
    Init {
        /// 配置文件路径
        #[arg(short = 'o', long, default_value = "~/.aginx/config.toml")]
        output: PathBuf,
    },

    /// 生成配对码（用于 App 绑定）
    Pair,

    /// 查看已绑定的设备
    Devices,

    /// 解绑设备
    Unbind,

    /// 显示设备硬件指纹
    Fingerprint,

    /// ACP 协议模式 (Agent Client Protocol)
    /// 用于 IDE 集成 (Zed, Cursor, VS Code 等)
    Acp {
        /// 使用 stdio 模式 (被 IDE 启动)
        #[arg(long)]
        stdio: bool,

        /// 指定默认 Agent ID (必须配置)
        #[arg(short = 'a', long)]
        agent: Option<String>,
    },

    /// 运行 MCP Server (用于 Agent-to-Agent 通信)
    McpServe {
        /// 指定默认 Agent ID
        #[arg(short = 'a', long)]
        agent_id: Option<String>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // 初始化日志
    init_logging(args.verbose, args.debug);

    // 处理子命令
    if let Some(cmd) = args.command {
        return handle_command(cmd).await;
    }

    // 加载配置
    let result = load_config(&args)?;
    let config_path = result.config_path.unwrap_or_else(get_default_config_path);
    let mut config = result.config;

    // 创建 AgentManager
    let mut agent_manager = AgentManager::from_config(&config);

    // 启动前检查：至少有一个 agent
    if !agent_manager.has_agents() {
        if agent::setup::needs_setup() {
            agent::setup::run_setup()?;
            // 重新加载 AgentManager
            agent_manager = AgentManager::from_config(&config);
        }
        if !agent_manager.has_agents() {
            tracing::error!("没有配置任何 agent，无法启动。请在 ~/.aginx/agents/ 目录下添加 aginx.toml 配置文件。");
            std::process::exit(1);
        }
    }

    // 根据模式启动
    match config.server.mode {
        ServerMode::Direct => {
            tracing::info!("运行模式: 直连 (Direct)");
            let config = Arc::new(config);
            print_startup_info(&config);
            let server = server::Server::new(config.clone(), agent_manager)?;
            tokio::select! {
                r = server.run() => r?,
                _ = tokio::signal::ctrl_c() => {
                    tracing::info!("收到 Ctrl+C，正在关闭...");
                }
            }
        }
        ServerMode::Relay => {
            tracing::info!("运行模式: 中继 (Relay)");

            // 检查是否需要申请 ID（首次启动）
            if !config.relay.has_id() {
                tracing::info!("========================================");
                tracing::info!("首次启动，正在向 API 申请 ID...");
                tracing::info!("========================================");

                // 向 API 申请 ID（HTTP POST）
                let registration = relay::register_id(&config.api.url).await?;

                // 更新配置
                config.relay.set_id(registration.id.clone());
                config.relay.token = Some(registration.token);

                // 保存配置
                if let Err(e) = save_config(&config, &config_path) {
                    tracing::error!("保存配置失败: {}", e);
                } else {
                    tracing::info!("配置已保存到: {:?}", config_path);
                }

                tracing::info!("========================================");
                tracing::info!("申请成功!");
                tracing::info!("ID: {}", config.relay.id.as_deref().unwrap_or("?"));
                tracing::info!("========================================");
            }

            let config = Arc::new(config);
            print_startup_info(&config);

            // 连接 relay
            let mut relay_client = relay::RelayClient::new(&config, agent_manager);
            tokio::select! {
                r = relay_client.connect() => r?,
                _ = tokio::signal::ctrl_c() => {
                    tracing::info!("收到 Ctrl+C，正在关闭...");
                }
            }
        }
    }

    Ok(())
}

fn init_logging(verbose: bool, debug: bool) {
    let level = if debug {
        tracing::Level::DEBUG
    } else if verbose {
        tracing::Level::INFO
    } else {
        tracing::Level::WARN
    };

    tracing_subscriber::fmt()
        .with_max_level(level)
        .with_target(false)
        .compact()
        .with_writer(std::io::stderr)
        .init();
}

fn load_config(args: &Args) -> anyhow::Result<config::LoadResult> {
    let cli_args = config::CliArgs {
        config: args.config.clone(),
        port: args.port,
        host: args.host.clone(),
        mode: args.mode.as_ref().map(|m| ServerMode::from(m.clone())),
        public_url: args.public_url.clone(),
    };
    config::load_config(&cli_args)
}

fn print_startup_info(config: &Config) {
    let address = match config.server.mode {
        ServerMode::Direct => {
            if let Some(ref url) = config.direct.public_url {
                url.clone()
            } else {
                format!("agent://{}:{}", config.server.host, config.server.port)
            }
        }
        ServerMode::Relay => {
            if let Some(ref id) = config.relay.id {
                format!("agent://{}.{}", id, config.relay.domain)
            } else {
                format!("agent://xxx.{} (未配置)", config.relay.domain)
            }
        }
    };

    tracing::info!("========================================");
    tracing::info!("aginx v{}", config.server.version);
    tracing::info!("运行模式: {:?}", config.server.mode);
    tracing::info!("访问地址: {}", address);
    tracing::info!("访问权限: {:?}", config.server.access);
    tracing::info!("========================================");
}

/// 处理子命令
async fn handle_command(cmd: Commands) -> anyhow::Result<()> {
    match cmd {
        Commands::Init { output } => {
            let path = shellexpand::tilde(&output.to_string_lossy()).to_string();
            let path = PathBuf::from(path);

            if path.exists() {
                println!("配置文件已存在: {:?}", path);
                return Ok(());
            }

            // 创建目录
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }

            // 写入默认配置
            let default_config = Config::default();
            let content = toml::to_string_pretty(&default_config)?;
            std::fs::write(&path, content)?;

            println!("配置文件已创建: {:?}", path);
        }
        Commands::Pair => {
            let manager = binding::get_binding_manager();
            let mut mgr = manager.lock().unwrap();
            let result = mgr.generate_pair_code();

            // 加载配置获取正确的地址
            let config_result = config::load_config(&config::CliArgs::default())?;
            let address = match config_result.config.server.mode {
                ServerMode::Relay => {
                    if let Some(ref id) = config_result.config.relay.id {
                        format!("agent://{}.{}", id, config_result.config.relay.domain)
                    } else {
                        let local_ip = local_ip_address::local_ip()
                            .map(|ip| ip.to_string())
                            .unwrap_or_else(|_| "<未知>".to_string());
                        format!("agent://{}", local_ip)
                    }
                }
                ServerMode::Direct => {
                    if let Some(ref url) = config_result.config.direct.public_url {
                        url.clone()
                    } else {
                        let local_ip = local_ip_address::local_ip()
                            .map(|ip| ip.to_string())
                            .unwrap_or_else(|_| "<未知>".to_string());
                        format!("agent://{}", local_ip)
                    }
                }
            };

            println!("========================================");
            println!("服务器地址: {}", address);
            println!("配对码: {}", result.code);
            println!("有效期: {} 秒 (5分钟)", result.expires_in);
            println!("========================================");
            println!("");
            println!("在 App 中输入服务器地址和配对码完成绑定");
        }
        Commands::Devices => {
            let manager = binding::get_binding_manager();
            let mgr = manager.lock().unwrap();

            if let Some(device) = mgr.get_bound_device() {
                println!("已绑定的设备:");
                println!("");
                let bound_time = chrono::DateTime::from_timestamp(device.bound_at, 0)
                    .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_else(|| "未知".to_string());
                println!("  ID: {}", device.id);
                println!("  名称: {}", device.name);
                println!("  绑定时间: {}", bound_time);
            } else {
                println!("没有已绑定的设备");
            }
        }
        Commands::Unbind => {
            let manager = binding::get_binding_manager();
            let mut mgr = manager.lock().unwrap();

            if mgr.get_bound_device().is_none() {
                println!("没有已绑定的设备");
                return Ok(());
            }

            // 1. 清除本地绑定
            mgr.unbind_all();
            drop(mgr); // 提前释放锁
            println!("本地绑定已清除");

            // 2. 尝试通知 aginx-api 注销
            let config_result = config::load_config(&config::CliArgs::default());
            if let Ok(config_result) = config_result {
                let cfg = &config_result.config;
                if let Some(ref token) = cfg.relay.token {
                    if let Err(e) = deregister_from_api(&cfg.api.url, token).await {
                        println!("警告: API 注销失败 ({})，本地绑定已清除", e);
                    } else {
                        println!("API 注销成功");
                        // 清除配置中的 relay id 和 token，下次启动会重新注册
                        if let Some(ref config_path) = config_result.config_path {
                            let mut new_config = config_result.config;
                            new_config.relay.id = None;
                            new_config.relay.token = None;
                            new_config.relay.url = None;
                            if let Err(e) = config::save_config(&new_config, config_path) {
                                println!("警告: 清除配置失败 ({})", e);
                            } else {
                                println!("Relay 配置已清除，下次启动将重新注册");
                            }
                        }
                    }
                }
            }

            println!("设备已解绑");
        }
        Commands::Fingerprint => {
            use crate::fingerprint::HardwareFingerprint;

            let fp = HardwareFingerprint::generate();

            println!("========================================");
            println!("设备硬件指纹");
            println!("========================================");
            println!("");
            println!("MAC 地址: {}", fp.mac_address.as_deref().unwrap_or("无法获取"));
            println!("硬盘序列号: {}", fp.disk_serial.as_deref().unwrap_or("无法获取"));
            println!("主板 UUID: {}", fp.motherboard_uuid.as_deref().unwrap_or("无法获取"));
            println!("");
            println!("设备指纹: {}", fp.fingerprint);
            println!("========================================");
        }
        Commands::Acp { stdio, agent } => {
            run_acp_mode(stdio, agent).await?;
        }
        Commands::McpServe { agent_id } => {
            run_mcp_serve(agent_id).await?;
        }
    }

    Ok(())
}

/// 运行 ACP 模式
async fn run_acp_mode(stdio: bool, default_agent: Option<String>) -> anyhow::Result<()> {
    use crate::agent::{SessionManager, SessionConfig};
    use crate::acp::run_acp_stdio;

    // 加载配置
    let config = config::load_config(&config::CliArgs::default())?.config;

    // 创建 AgentManager
    let mut agent_manager = Arc::new(AgentManager::from_config(&config));

    // 启动前检查：至少有一个 agent
    if !agent_manager.has_agents() {
        if agent::setup::needs_setup() {
            agent::setup::run_setup()?;
            agent_manager = Arc::new(AgentManager::from_config(&config));
        }
        if !agent_manager.has_agents() {
            tracing::error!("没有配置任何 agent，无法启动。请在 ~/.aginx/agents/ 目录下添加 aginx.toml 配置文件。");
            std::process::exit(1);
        }
    }

    // 创建 SessionManager
    let session_config = SessionConfig {
        max_concurrent: config.server.max_concurrent_sessions,
        timeout_seconds: config.server.session_timeout_seconds,
    };
    let session_manager = Arc::new(SessionManager::new(session_config));

    // 启动超时清理任务
    let cleanup_handle = session_manager.clone().start_cleanup_task();

    if let Some(ref agent) = default_agent {
        tracing::info!("ACP 模式启动, 默认 Agent: {}", agent);
    } else {
        tracing::info!("ACP 模式启动, Agent 需要通过请求指定");
    }

    if stdio {
        // stdio 模式 - 被 IDE 启动
        run_acp_stdio(agent_manager, session_manager).await;
    } else {
        // 交互模式 - 用于测试
        tracing::error!("ACP 交互模式尚未实现，请使用 --stdio");
        println!("ACP 交互模式尚未实现");
        println!("请使用: aginx acp --stdio");
    }

    // 清理
    cleanup_handle.abort();

    Ok(())
}

/// 调用 aginx-api 注销实例
async fn deregister_from_api(api_url: &str, token: &str) -> anyhow::Result<()> {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/v1/instances/deregister", api_url))
        .bearer_auth(token)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!("API 注销失败 ({}): {}", status, body));
    }

    tracing::info!("aginx-api 实例已注销");
    Ok(())
}

/// Run MCP server for agent-to-agent communication over stdio.
async fn run_mcp_serve(_agent_id: Option<String>) -> anyhow::Result<()> {
    use crate::agent::{SessionManager, SessionConfig};

    let config = config::load_config(&config::CliArgs::default())?.config;
    let agent_manager = Arc::new(AgentManager::from_config(&config));
    let session_config = SessionConfig {
        max_concurrent: config.server.max_concurrent_sessions,
        timeout_seconds: config.server.session_timeout_seconds,
    };
    let session_manager = Arc::new(SessionManager::new(session_config));

    let orchestrator = Arc::new(crate::acp::orchestrator::AgentOrchestrator::new(
        agent_manager,
        session_manager,
    ));

    // Simple MCP server: read JSON-RPC from stdin, write responses to stdout
    tracing::info!("MCP server starting on stdio");

    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let stdin = tokio::io::BufReader::new(tokio::io::stdin());
    let mut stdout = tokio::io::BufWriter::new(tokio::io::stdout());
    let mut lines = stdin.lines();

    // MCP initialize
    while let Ok(Some(line)) = lines.next_line().await {
        let msg: serde_json::Value = match serde_json::from_str(&line) {
            Ok(m) => m,
            Err(_) => continue,
        };

        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let id = msg.get("id").cloned();

        match method {
            "initialize" => {
                let resp = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "protocolVersion": "2024-11-05",
                        "capabilities": { "tools": {} },
                        "serverInfo": { "name": "aginx-mcp", "version": env!("CARGO_PKG_VERSION") }
                    }
                });
                stdout.write_all(serde_json::to_string(&resp).unwrap().as_bytes()).await?;
                stdout.write_all(b"\n").await?;
                stdout.flush().await?;
            }
            "tools/list" => {
                let tools = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "tools": [
                            {
                                "name": "aginx_list_agents",
                                "description": "List all available agents on this aginx instance",
                                "inputSchema": { "type": "object", "properties": {} }
                            },
                            {
                                "name": "aginx_create_sub_agent",
                                "description": "Create a sub-agent session for agent-to-agent communication",
                                "inputSchema": {
                                    "type": "object",
                                    "properties": {
                                        "agentId": { "type": "string", "description": "Target agent ID" },
                                        "workdir": { "type": "string", "description": "Working directory for the sub-agent" }
                                    },
                                    "required": ["agentId"]
                                }
                            },
                            {
                                "name": "aginx_cancel_agent",
                                "description": "Cancel a running sub-agent",
                                "inputSchema": {
                                    "type": "object",
                                    "properties": {
                                        "subSessionId": { "type": "string", "description": "The sub-agent session ID" }
                                    },
                                    "required": ["subSessionId"]
                                }
                            },
                            {
                                "name": "aginx_list_sub_agents",
                                "description": "List all sub-agents for the current parent session",
                                "inputSchema": {
                                    "type": "object",
                                    "properties": {
                                        "parentSessionId": { "type": "string", "description": "The parent session ID" }
                                    },
                                    "required": ["parentSessionId"]
                                }
                            }
                        ]
                    }
                });
                stdout.write_all(serde_json::to_string(&tools).unwrap().as_bytes()).await?;
                stdout.write_all(b"\n").await?;
                stdout.flush().await?;
            }
            "tools/call" => {
                let tool_name = msg.get("params")
                    .and_then(|p| p.get("name"))
                    .and_then(|n| n.as_str())
                    .unwrap_or("");
                let args = msg.get("params")
                    .and_then(|p| p.get("arguments"))
                    .cloned()
                    .unwrap_or(serde_json::json!({}));

                let result = match tool_name {
                    "aginx_list_agents" => {
                        let agents = orchestrator.list_available_agents().await;
                        serde_json::json!({ "content": [{ "type": "text", "text": serde_json::to_string_pretty(&agents).unwrap_or_default() }] })
                    }
                    "aginx_create_sub_agent" => {
                        let agent_id = args.get("agentId").and_then(|v| v.as_str()).unwrap_or("");
                        let workdir = args.get("workdir").and_then(|v| v.as_str());
                        match orchestrator.create_sub_agent("mcp-parent", agent_id, workdir).await {
                            Ok(session_id) => serde_json::json!({
                                "content": [{ "type": "text", "text": format!("Sub-agent created: {}", session_id) }]
                            }),
                            Err(e) => serde_json::json!({
                                "content": [{ "type": "text", "text": format!("Error: {}", e) }],
                                "isError": true
                            }),
                        }
                    }
                    "aginx_cancel_agent" => {
                        let sub_session_id = args.get("subSessionId").and_then(|v| v.as_str()).unwrap_or("");
                        match orchestrator.cancel_sub_agent(sub_session_id).await {
                            Ok(()) => serde_json::json!({
                                "content": [{ "type": "text", "text": "Sub-agent canceled" }]
                            }),
                            Err(e) => serde_json::json!({
                                "content": [{ "type": "text", "text": format!("Error: {}", e) }],
                                "isError": true
                            }),
                        }
                    }
                    "aginx_list_sub_agents" => {
                        let parent = args.get("parentSessionId").and_then(|v| v.as_str()).unwrap_or("");
                        let subs = orchestrator.list_sub_agents(parent).await;
                        serde_json::json!({
                            "content": [{ "type": "text", "text": serde_json::to_string_pretty(&subs).unwrap_or_default() }]
                        })
                    }
                    _ => serde_json::json!({
                        "content": [{ "type": "text", "text": format!("Unknown tool: {}", tool_name) }],
                        "isError": true
                    }),
                };

                let resp = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": result
                });
                stdout.write_all(serde_json::to_string(&resp).unwrap().as_bytes()).await?;
                stdout.write_all(b"\n").await?;
                stdout.flush().await?;
            }
            "notifications/initialized" => {
                // No response needed for notifications
                continue;
            }
            _ => {
                let resp = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32601, "message": format!("Method not found: {}", method) }
                });
                stdout.write_all(serde_json::to_string(&resp).unwrap().as_bytes()).await?;
                stdout.write_all(b"\n").await?;
                stdout.flush().await?;
            }
        }
    }

    Ok(())
}
