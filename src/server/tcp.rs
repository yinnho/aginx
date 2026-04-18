//! TCP Server for aginx

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio::io::AsyncWriteExt;

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
    pub fn new(config: Arc<Config>, agent_manager: AgentManager) -> anyhow::Result<Self> {
        let conn_semaphore = Arc::new(Semaphore::new(config.server.max_connections));
        Ok(Self {
            config,
            agent_manager,
            conn_semaphore,
        })
    }

    pub async fn run(self) -> anyhow::Result<()> {
        let addr_str = if self.config.server.host.contains(':') {
            format!("[{}]:{}", self.config.server.host, self.config.server.port)
        } else {
            format!("{}:{}", self.config.server.host, self.config.server.port)
        };
        let addr: SocketAddr = addr_str.parse()
            .map_err(|e| anyhow::anyhow!("Invalid address {}: {}", addr_str, e))?;

        let listener = TcpListener::bind(addr).await?;
        tracing::info!("Listening on {}", addr);

        loop {
            let (stream, peer_addr) = listener.accept().await?;

            let permit = match self.conn_semaphore.clone().try_acquire_owned() {
                Ok(permit) => permit,
                Err(_) => {
                    tracing::warn!("Connection limit reached, rejecting {}", peer_addr);
                    let error_json = r#"{"jsonrpc":"2.0","error":{"code":-32000,"message":"Connection limit reached"}}"#;
                    let mut s = stream;
                    let _ = s.write_all(error_json.as_bytes()).await;
                    let _ = s.write_all(b"\n").await;
                    continue;
                }
            };

            let handler = Handler::new(self.config.clone(), self.agent_manager.clone());

            tokio::spawn(async move {
                if let Err(e) = handler.handle(stream, peer_addr).await {
                    tracing::error!("Connection error from {}: {}", peer_addr, e);
                }
                drop(permit);
            });
        }
    }
}
