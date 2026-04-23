//! Configuration loader

use std::path::{Path, PathBuf};

use super::{Config, ServerMode};

/// CLI arguments for config override
#[derive(Default)]
pub struct CliArgs {
    pub config: Option<PathBuf>,
    pub port: Option<u16>,
    pub host: Option<String>,
    pub mode: Option<ServerMode>,
    pub public_url: Option<String>,
}

/// Load result with config path
pub struct LoadResult {
    pub config: Config,
    pub config_path: Option<PathBuf>,
}

/// Load configuration
pub fn load_config(args: &CliArgs) -> anyhow::Result<LoadResult> {
    // Find config file path
    let config_path = args.config.clone().or_else(find_default_config);

    let mut config = if let Some(ref path) = config_path {
        if path.exists() {
            tracing::info!("Loading config from: {:?}", path);
            load_from_file(path)?
        } else {
            tracing::info!("Config file not found, using defaults: {:?}", path);
            Config::default()
        }
    } else {
        tracing::info!("No config file specified, using defaults");
        Config::default()
    };

    // Apply CLI overrides
    // Priority: CLI args > config file > defaults
    // Special case: --public-url implies Direct mode unless --mode is explicitly set
    if let Some(port) = args.port {
        config.server.port = port;
    }
    if let Some(host) = &args.host {
        config.server.host = host.clone();
    }
    if let Some(public_url) = &args.public_url {
        // public_url implies Direct mode (only if mode not explicitly set)
        if args.mode.is_none() {
            config.server.mode = ServerMode::Direct;
        }
        config.direct.public_url = Some(public_url.clone());
    }
    // Mode override should come after public_url to allow explicit override
    if let Some(mode) = &args.mode {
        config.server.mode = mode.clone();
    }

    config.validate()?;
    Ok(LoadResult { config, config_path })
}

/// Load config from file
fn load_from_file(path: &Path) -> anyhow::Result<Config> {
    let content = std::fs::read_to_string(path)?;
    let config: Config = toml::from_str(&content)?;
    Ok(config)
}

/// Find default config file
fn find_default_config() -> Option<PathBuf> {
    let candidates = vec![
        "./aginx.toml",
        "./.aginx/config.toml",
        "~/.aginx/config.toml",
        "/etc/aginx/config.toml",
    ];

    for candidate in candidates {
        let path = shellexpand::tilde(candidate).to_string();
        let path = PathBuf::from(path);
        if path.exists() {
            return Some(path);
        }
    }

    None
}

/// Get default config file path (for saving)
pub fn get_default_config_path() -> PathBuf {
    PathBuf::from(shellexpand::tilde("~/.aginx/config.toml").to_string())
}

/// Save config to file
pub fn save_config(config: &Config, path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let content = toml::to_string_pretty(config)?;
    std::fs::write(path, content)?;

    tracing::info!("Config saved to: {:?}", path);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.server.port, 86);
        assert_eq!(config.server.host, "0.0.0.0");
        assert_eq!(config.server.mode, ServerMode::Relay); // 默认是 relay 模式
    }

    #[test]
    fn test_config_serialization() {
        let config = Config::default();
        let toml = toml::to_string_pretty(&config).unwrap();
        let parsed: Config = toml::from_str(&toml).unwrap();
        assert_eq!(config.server.port, parsed.server.port);
    }

    #[test]
    fn test_relay_config_set_id() {
        let mut config = Config::default();
        config.relay.set_id("abc123".to_string());
        assert_eq!(config.relay.id, Some("abc123".to_string()));
        assert_eq!(config.relay.url, Some("abc123.relay.yinnho.cn:8443".to_string()));
    }
}
