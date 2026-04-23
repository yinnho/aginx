//! aginx — Agent Internet Gateway
//!
//! Route requests to CLI agents, like nginx routes to websites.
//!
//! Usage:
//!   aginx                    # Start with config (default: relay mode)
//!   aginx --mode direct      # Direct TCP mode, listen on port 86
//!   aginx init               # Generate config file
//!   aginx pair               # Generate pairing code
//!   aginx devices            # List bound devices

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

#[derive(Parser, Debug)]
#[command(name = "aginx")]
#[command(version, about = "Agent Internet Gateway", long_about = None)]
struct Args {
    #[command(subcommand)]
    command: Option<Commands>,

    #[arg(short = 'c', long, value_name = "FILE")]
    config: Option<PathBuf>,

    #[arg(short = 'm', long, value_enum)]
    mode: Option<ModeArg>,

    #[arg(short = 'p', long)]
    port: Option<u16>,

    #[arg(short = 'H', long)]
    host: Option<String>,

    #[arg(long, value_name = "URL")]
    public_url: Option<String>,

    #[arg(short = 'v', long)]
    verbose: bool,

    #[arg(short = 'd', long)]
    debug: bool,
}

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
    /// Generate config file
    Init {
        #[arg(short = 'o', long, default_value = "~/.aginx/config.toml")]
        output: PathBuf,
    },
    /// Generate pairing code
    Pair,
    /// List bound devices
    Devices,
    /// Unbind device
    Unbind,
    /// Show hardware fingerprint
    Fingerprint,
    /// Generate authorization token for a client
    Auth {
        /// Client name
        #[arg(short = 'n', long)]
        name: String,
        /// Allowed agents (comma-separated, empty = all)
        #[arg(short = 'a', long)]
        agents: Option<String>,
        /// Allowed methods (comma-separated, empty = all)
        #[arg(short = 'm', long)]
        methods: Option<String>,
        /// Allow system methods
        #[arg(long)]
        system: bool,
        /// Expiration in days (default: 30)
        #[arg(short = 'e', long, default_value = "30")]
        expire_days: i64,
    },
    /// List authorized clients
    Auths,
    /// Revoke an authorized client
    Revoke {
        /// Client ID to revoke
        id: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    init_logging(args.verbose, args.debug);

    if let Some(cmd) = args.command {
        return handle_command(cmd).await;
    }

    // Load config
    let result = load_config(&args)?;
    let config_path = result.config_path.unwrap_or_else(get_default_config_path);
    let mut config = result.config;

    // Create AgentManager
    let mut agent_manager = AgentManager::from_config(&config);

    if !agent_manager.has_agents() {
        if agent::setup::needs_setup() {
            agent::setup::run_setup()?;
            agent_manager = AgentManager::from_config(&config);
        }
        if !agent_manager.has_agents() {
            tracing::error!("No agents configured. Add aginx.toml to ~/.aginx/agents/");
            std::process::exit(1);
        }
    }

    match config.server.mode {
        ServerMode::Direct => {
            tracing::info!("Mode: Direct");
            let config = Arc::new(config);
            print_startup_info(&config);
            let server = server::Server::new(config.clone(), agent_manager)?;
            tokio::select! {
                r = server.run() => r?,
                _ = tokio::signal::ctrl_c() => {
                    tracing::info!("Shutting down...");
                }
            }
        }
        ServerMode::Relay => {
            tracing::info!("Mode: Relay");

            if !config.relay.has_id() {
                tracing::info!("First run, registering with API...");
                let registration = relay::register_id(&config.api.url).await?;
                config.relay.set_id(registration.id.clone());
                config.relay.token = Some(registration.token);

                if let Err(e) = save_config(&config, &config_path) {
                    tracing::error!("Failed to save config: {}", e);
                } else {
                    tracing::info!("Config saved to: {:?}", config_path);
                }
            }

            let config = Arc::new(config);
            print_startup_info(&config);

            let mut relay_client = relay::RelayClient::new(&config, agent_manager);
            tokio::select! {
                r = relay_client.connect() => r?,
                _ = tokio::signal::ctrl_c() => {
                    tracing::info!("Shutting down...");
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
            config.direct.public_url.clone()
                .unwrap_or_else(|| format!("agent://{}:{}", config.server.host, config.server.port))
        }
        ServerMode::Relay => {
            if let Some(ref id) = config.relay.id {
                format!("agent://{}.{}", id, config.relay.domain)
            } else {
                format!("agent://xxx.{} (not configured)", config.relay.domain)
            }
        }
    };

    tracing::info!("========================================");
    tracing::info!("aginx v{}", config.server.version);
    tracing::info!("Mode: {:?}", config.server.mode);
    tracing::info!("Address: {}", address);
    tracing::info!("Access: {:?}", config.server.access);
    tracing::info!("========================================");
}

async fn handle_command(cmd: Commands) -> anyhow::Result<()> {
    match cmd {
        Commands::Init { output } => {
            let path = shellexpand::tilde(&output.to_string_lossy()).to_string();
            let path = PathBuf::from(path);

            if path.exists() {
                println!("Config already exists: {:?}", path);
                return Ok(());
            }

            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }

            let default_config = Config::default();
            let content = toml::to_string_pretty(&default_config)?;
            std::fs::write(&path, content)?;

            println!("Config created: {:?}", path);
        }
        Commands::Pair => {
            let manager = binding::get_binding_manager();
            let mut mgr = manager.lock().unwrap();
            let result = mgr.generate_pair_code();

            let config_result = config::load_config(&config::CliArgs::default())?;
            let address = match config_result.config.server.mode {
                ServerMode::Relay => {
                    if let Some(ref id) = config_result.config.relay.id {
                        format!("agent://{}.{}", id, config_result.config.relay.domain)
                    } else {
                        format!("agent://{}", local_ip_address::local_ip()
                            .map(|ip| ip.to_string())
                            .unwrap_or_else(|_| "unknown".to_string()))
                    }
                }
                ServerMode::Direct => {
                    config_result.config.direct.public_url.clone()
                        .unwrap_or_else(|| format!("agent://{}", local_ip_address::local_ip()
                            .map(|ip| ip.to_string())
                            .unwrap_or_else(|_| "unknown".to_string())))
                }
            };

            println!("========================================");
            println!("Server: {}", address);
            println!("Pair code: {}", result.code);
            println!("Expires: {}s", result.expires_in);
            println!("========================================");
        }
        Commands::Devices => {
            let manager = binding::get_binding_manager();
            let mgr = manager.lock().unwrap();

            if let Some(device) = mgr.get_bound_device() {
                let bound_time = chrono::DateTime::from_timestamp(device.bound_at, 0)
                    .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                println!("Bound device:");
                println!("  ID: {}", device.id);
                println!("  Name: {}", device.name);
                println!("  Bound: {}", bound_time);
            } else {
                println!("No bound devices");
            }
        }
        Commands::Unbind => {
            let manager = binding::get_binding_manager();
            let mut mgr = manager.lock().unwrap();

            if mgr.get_bound_device().is_none() {
                println!("No bound devices");
                return Ok(());
            }

            mgr.unbind_all();
            drop(mgr);
            println!("Device unbound");

            let config_result = config::load_config(&config::CliArgs::default());
            if let Ok(config_result) = config_result {
                let cfg = &config_result.config;
                if let Some(ref token) = cfg.relay.token {
                    if let Err(e) = deregister_from_api(&cfg.api.url, token).await {
                        println!("Warning: API deregister failed ({})", e);
                    } else if let Some(ref config_path) = config_result.config_path {
                        let mut new_config = config_result.config;
                        new_config.relay.id = None;
                        new_config.relay.token = None;
                        new_config.relay.url = None;
                        let _ = config::save_config(&new_config, config_path);
                    }
                }
            }
        }
        Commands::Fingerprint => {
            use crate::fingerprint::HardwareFingerprint;
            let fp = HardwareFingerprint::generate();
            println!("Fingerprint: {}", fp.fingerprint);
        }
        Commands::Auth { name, agents, methods, system, expire_days } => {
            let config_result = config::load_config(&config::CliArgs::default())?;
            let jwt_secret = config_result.config.auth.jwt_secret
                .ok_or_else(|| anyhow::anyhow!("JWT secret not configured. Set auth.jwt_secret in config"))?;

            let client_id = format!("client-{}", uuid::Uuid::new_v4().to_string().split('-').next().unwrap_or("unknown"));
            let now = chrono::Utc::now().timestamp();
            let exp = now + expire_days * 86400;

            let allowed_agents: Vec<String> = agents.map(|s| s.split(',').map(|a| a.trim().to_string()).collect()).unwrap_or_default();
            let allowed_methods: Vec<String> = methods.map(|s| s.split(',').map(|m| m.trim().to_string()).collect()).unwrap_or_default();

            let claims = crate::auth::AuthClientClaims {
                sub: client_id.clone(),
                name: name.clone(),
                agents: allowed_agents.clone(),
                methods: allowed_methods.clone(),
                sys: system,
                iat: now,
                exp,
            };

            let token = crate::auth::generate_auth_client_jwt(&claims, &jwt_secret)
                .map_err(|e| anyhow::anyhow!("JWT generation failed: {}", e))?;

            // Store in auth manager
            let auth_mgr = crate::auth::get_auth_manager();
            let mut mgr = auth_mgr.lock().unwrap();
            mgr.add_client(crate::auth::AuthorizedClient {
                id: client_id.clone(),
                name: name.clone(),
                token: token.clone(),
                created_at: now,
                expires_at: Some(exp),
                allowed_agents,
                allowed_methods,
                allow_system: system,
            })?;
            drop(mgr);

            println!("========================================");
            println!("Authorization token generated");
            println!("Client ID: {}", client_id);
            println!("Name: {}", name);
            println!("Expires: {} days", expire_days);
            println!("");
            println!("Token:");
            println!("{}", token);
            println!("========================================");
        }
        Commands::Auths => {
            let auth_mgr = crate::auth::get_auth_manager();
            let mgr = auth_mgr.lock().unwrap();
            let clients = mgr.list_clients();
            let now = chrono::Utc::now().timestamp();

            if clients.is_empty() {
                println!("No authorized clients");
                return Ok(());
            }

            println!("Authorized clients:");
            for client in clients {
                let status = match client.expires_at {
                    Some(exp) if now >= exp => "EXPIRED",
                    _ => "active",
                };
                println!("  {} | {} | {}", client.id, client.name, status);
            }
        }
        Commands::Revoke { id } => {
            let auth_mgr = crate::auth::get_auth_manager();
            let mut mgr = auth_mgr.lock().unwrap();
            if mgr.remove_client(&id) {
                println!("Client {} revoked", id);
            } else {
                println!("Client {} not found", id);
            }
        }
    }

    Ok(())
}

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
        return Err(anyhow::anyhow!("API deregister failed ({}): {}", status, body));
    }

    Ok(())
}
