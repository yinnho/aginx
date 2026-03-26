//! aginx - Agent Protocol 实现
//!
//! 让用户可以像访问网站一样访问 Agent
//!
//! Usage:
//!   aginx                              # 根据 config.toml 启动（默认 relay 模式，自动申请 ID）
//!   aginx --mode direct                # 直连模式，监听 TCP 86

mod config;
mod protocol;
mod server;
mod agent;
mod relay;
mod binding;
mod acp;
mod fingerprint;

use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand, ValueEnum};
use crate::config::{Config, ServerMode, save_config, get_default_config_path};
use crate::agent::AgentManager;
use crate::binding::BindingManager;

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
    #[arg(short = 'p', long, default_value = "86")]
    port: u16,

    /// 绑定地址
    #[arg(short = 'H', long, default_value = "0.0.0.0")]
    host: String,

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

    /// 查看状态
    Status,

    /// 生成配对码（用于 App 绑定）
    Pair,

    /// 查看已绑定的设备
    Devices,

    /// 解绑设备
    Unbind {
        /// 设备 ID
        device_id: String,
    },

    /// 显示设备硬件指纹
    Fingerprint,

    /// ACP 协议模式 (Agent Client Protocol)
    /// 用于 IDE 集成 (Zed, Cursor, VS Code 等)
    Acp {
        /// 使用 stdio 模式 (被 IDE 启动)
        #[arg(long)]
        stdio: bool,

        /// 指定默认 Agent ID
        #[arg(short = 'a', long, default_value = "claude")]
        agent: String,
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
    let agent_manager = AgentManager::from_config(&config);

    // 根据模式启动
    match config.server.mode {
        ServerMode::Direct => {
            tracing::info!("运行模式: 直连 (Direct)");
            let config = Arc::new(config);
            print_startup_info(&config);
            let server = server::Server::new(config.clone(), agent_manager)?;
            server.run().await?;
        }
        ServerMode::Relay => {
            tracing::info!("运行模式: 中继 (Relay)");

            // 检查是否需要申请 ID（首次启动）
            if !config.relay.has_id() {
                tracing::info!("========================================");
                tracing::info!("首次启动，正在向 relay.yinnho.cn 申请 ID...");
                tracing::info!("========================================");

                // 向 relay 申请 ID
                let registration = relay::register_id().await?;

                // 更新配置
                config.relay.set_id(registration.id.clone());

                // 保存配置
                if let Err(e) = save_config(&config, &config_path) {
                    tracing::error!("保存配置失败: {}", e);
                } else {
                    tracing::info!("配置已保存到: {:?}", config_path);
                }

                tracing::info!("========================================");
                tracing::info!("申请成功!");
                tracing::info!("ID: {}", registration.id);
                tracing::info!("URL: {}", registration.url);
                tracing::info!("========================================");
            }

            let config = Arc::new(config);
            print_startup_info(&config);

            // 连接 relay
            let mut relay_client = relay::RelayClient::new(&config, agent_manager);
            relay_client.connect(config).await?;
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
        .init();
}

fn load_config(args: &Args) -> anyhow::Result<config::LoadResult> {
    let cli_args = config::CliArgs {
        config: args.config.clone(),
        port: Some(args.port),
        host: Some(args.host.clone()),
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
                format!("agent://{}.relay.yinnho.cn", id)
            } else {
                "agent://xxx.relay.yinnho.cn (未配置)".to_string()
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
        Commands::Status => {
            println!("状态查看功能尚未实现");
        }
        Commands::Pair => {
            let mut manager = BindingManager::new();
            let result = manager.generate_pair_code();

            println!("========================================");
            println!("配对码: {}", result.code);
            println!("有效期: {} 秒 (5分钟)", result.expires_in);
            println!("========================================");
            println!("");
            println!("在 App 中输入此配对码完成绑定");
        }
        Commands::Devices => {
            let manager = BindingManager::new();
            let devices = manager.list_devices();

            if devices.is_empty() {
                println!("没有已绑定的设备");
            } else {
                println!("已绑定的设备:");
                println!("");
                for device in devices {
                    let bound_time = chrono::DateTime::from_timestamp(device.bound_at, 0)
                        .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                        .unwrap_or_else(|| "未知".to_string());
                    println!("  ID: {}", device.id);
                    println!("  名称: {}", device.name);
                    println!("  绑定时间: {}", bound_time);
                    println!("  ---");
                }
            }
        }
        Commands::Unbind { device_id } => {
            let mut manager = BindingManager::new();

            if manager.unbind_device(&device_id) {
                println!("设备 {} 已解绑", device_id);
            } else {
                println!("设备 {} 不存在", device_id);
            }
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
    }

    Ok(())
}

/// 运行 ACP 模式
async fn run_acp_mode(stdio: bool, default_agent: String) -> anyhow::Result<()> {
    use crate::agent::{SessionManager, SessionConfig};
    use crate::acp::run_acp_stdio;

    // 加载配置
    let config = config::load_config(&config::CliArgs::default())?.config;

    // 创建 AgentManager
    let agent_manager = Arc::new(AgentManager::from_config(&config));

    // 创建 SessionManager
    let session_config = SessionConfig {
        max_concurrent: 10,
        timeout_seconds: 1800,
    };
    let session_manager = Arc::new(SessionManager::new(session_config));

    // 启动超时清理任务
    let cleanup_handle = session_manager.clone().start_cleanup_task();

    tracing::info!("ACP 模式启动, 默认 Agent: {}", default_agent);

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
