//! TCP Server for aginx

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::TcpListener;
use tokio::sync::Semaphore;

use crate::config::Config;
use crate::agent::AgentManager;
use super::handler::Handler;

/// aginx Server
pub struct Server {
    config: Arc<Config>,
    agent_manager: AgentManager,
    conn_semaphore: Arc<Semaphore>,
}

impl Server {
    /// Create a new server
    pub fn new(config: Arc<Config>, agent_manager: AgentManager) -> anyhow::Result<Self> {
        let conn_semaphore = Arc::new(Semaphore::new(config.server.max_connections));

        Ok(Self {
            config,
            agent_manager,
            conn_semaphore,
        })
    }

    /// Run the server
    pub async fn run(self) -> anyhow::Result<()> {
        let addr: SocketAddr = format!("{}:{}", self.config.server.host, self.config.server.port)
            .parse()
            .expect("Invalid address");

        let listener = TcpListener::bind(addr).await?;
        tracing::info!("Listening on {}", addr);

        loop {
            // Accept new connection
            let (stream, peer_addr) = listener.accept().await?;

            // Check connection limit
            let permit = match self.conn_semaphore.clone().try_acquire_owned() {
                Ok(permit) => permit,
                Err(_) => {
                    tracing::warn!("Connection limit reached, rejecting {}", peer_addr);
                    continue;
                }
            };

            tracing::debug!("New connection from {}", peer_addr);

            // Create handler
            let handler = Handler::new(
                self.config.clone(),
                self.agent_manager.clone(),
            );

            // Spawn handler
            tokio::spawn(async move {
                if let Err(e) = handler.handle(stream, peer_addr).await {
                    tracing::error!("Connection error from {}: {}", peer_addr, e);
                }
                drop(permit);
            });
        }
    }
}
