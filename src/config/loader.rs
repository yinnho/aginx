//! Configuration loader

use std::path::{Path, PathBuf};

use super::{Config, ServerMode};

/// CLI arguments for config override
pub struct CliArgs {
    pub config: Option<PathBuf>,
    pub port: Option<u16>,
    pub host: Option<String>,
    pub mode: Option<ServerMode>,
    pub relay_url: Option<String>,
    pub public_url: Option<String>,
    pub verbose: bool,
    pub debug: bool,
}

/// Load configuration
pub fn load_config(args: &CliArgs) -> anyhow::Result<Config> {
    // Find config file path
    let config_path = args.config.clone().or_else(find_default_config);

    let mut config = if let Some(path) = config_path {
        if path.exists() {
            tracing::info!("Loading config from: {:?}", path);
            load_from_file(&path)?
        } else {
            tracing::warn!("Config file not found, using defaults: {:?}", path);
            Config::default()
        }
    } else {
        tracing::info!("No config file specified, using defaults");
        Config::default()
    };

    // Apply CLI overrides
    if let Some(port) = args.port {
        config.server.port = port;
    }
    if let Some(host) = &args.host {
        config.server.host = host.clone();
    }
    if let Some(mode) = &args.mode {
        config.server.mode = mode.clone();
    }
    if let Some(relay_url) = &args.relay_url {
        config.server.mode = ServerMode::Relay;
        config.relay.url = Some(relay_url.clone());
    }
    if let Some(public_url) = &args.public_url {
        config.server.mode = ServerMode::Direct;
        config.direct.public_url = Some(public_url.clone());
    }

    Ok(config)
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
    }

    #[test]
    fn test_config_serialization() {
        let config = Config::default();
        let toml = toml::to_string_pretty(&config).unwrap();
        let parsed: Config = toml::from_str(&toml).unwrap();
        assert_eq!(config.server.port, parsed.server.port);
    }
}
