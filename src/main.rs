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
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // 初始化日志
    init_logging(args.verbose, args.debug);

    // 处理子命令
    if let Some(cmd) = args.command {
        return handle_command(cmd);
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
fn handle_command(cmd: Commands) -> anyhow::Result<()> {
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
    }

    Ok(())
}
