use std::net::SocketAddr;

use anyhow::{Context, Result};
use clap::Parser;

/// Legacy CLI/GUI config — used when running the engine in-process.
#[derive(Parser, Debug, Clone)]
#[command(author, version, about)]
pub struct AppConfig {
    #[arg(long, env = "DEV_MODE")]
    pub paper: bool,

    #[arg(short, long)]
    pub verbose: bool,
}

impl AppConfig {
    pub fn load() -> Self {
        dotenvy::dotenv().ok();
        Self::parse()
    }
}

/// Configuration for the headless `ingot-server` daemon.
#[derive(Parser, Debug, Clone)]
#[command(author, version, about = "Ingot trading server")]
pub struct ServerConfig {
    /// Address to bind the HTTP/WS server to.
    #[arg(long, env = "INGOT_BIND", default_value = "127.0.0.1")]
    pub bind: String,

    /// Port for the HTTP/WS server.
    #[arg(long, env = "INGOT_PORT", default_value_t = 3847)]
    pub port: u16,

    /// Run with the paper-trading (simulated) exchange.
    #[arg(long, env = "DEV_MODE")]
    pub paper: bool,

    /// Enable verbose logging output.
    #[arg(short, long)]
    pub verbose: bool,
}

impl ServerConfig {
    /// Load configuration from CLI args and environment.
    pub fn load() -> Self {
        dotenvy::dotenv().ok();
        Self::parse()
    }

    /// Resolve bind address + port into a [`SocketAddr`].
    pub fn socket_addr(&self) -> Result<SocketAddr> {
        let addr: SocketAddr = format!("{}:{}", self.bind, self.port)
            .parse()
            .with_context(|| format!("invalid bind address: {}:{}", self.bind, self.port))?;
        Ok(addr)
    }
}

/// Configuration for thin clients (`ingot-cli`, `ingot-gui`).
#[derive(Parser, Debug, Clone)]
#[command(author, version, about = "Ingot trading client")]
pub struct ClientConfig {
    /// URL of the running `ingot-server` instance.
    #[arg(
        long,
        env = "INGOT_SERVER_URL",
        default_value = "http://127.0.0.1:3847"
    )]
    pub server_url: String,

    /// Enable verbose logging output.
    #[arg(short, long)]
    pub verbose: bool,
}

impl ClientConfig {
    /// Load configuration from CLI args and environment.
    pub fn load() -> Self {
        dotenvy::dotenv().ok();
        Self::parse()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_server_config_defaults() {
        let config = ServerConfig::parse_from(["test"]);
        assert_eq!(config.bind, "127.0.0.1");
        assert_eq!(config.port, 3847);
        assert!(!config.paper);
        assert!(!config.verbose);
    }

    #[test]
    fn test_server_config_custom_args() {
        let config =
            ServerConfig::parse_from(["test", "--bind", "0.0.0.0", "--port", "8080", "--paper"]);
        assert_eq!(config.bind, "0.0.0.0");
        assert_eq!(config.port, 8080);
        assert!(config.paper);
    }

    #[test]
    fn test_server_config_socket_addr() -> Result<()> {
        let config = ServerConfig::parse_from(["test"]);
        let addr = config.socket_addr()?;
        assert_eq!(addr.ip().to_string(), "127.0.0.1");
        assert_eq!(addr.port(), 3847);
        Ok(())
    }

    #[test]
    fn test_server_config_socket_addr_invalid() {
        let config = ServerConfig {
            bind: "not-a-valid-ip".into(),
            port: 3847,
            paper: false,
            verbose: false,
        };
        assert!(config.socket_addr().is_err());
    }

    #[test]
    fn test_client_config_defaults() {
        let config = ClientConfig::parse_from(["test"]);
        assert_eq!(config.server_url, "http://127.0.0.1:3847");
        assert!(!config.verbose);
    }

    #[test]
    fn test_client_config_custom_url() {
        let config = ClientConfig::parse_from(["test", "--server-url", "http://10.0.0.1:9000"]);
        assert_eq!(config.server_url, "http://10.0.0.1:9000");
    }
}
