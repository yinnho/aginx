//! 测试客户端 - 连接到 agent:// URL
//!
//! Usage:
//!   cargo run --example test_client_relay agent://rcs0aj94.relay.yinnho.cn

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

fn parse_agent_url(url: &str) -> Option<(String, String)> {
    // agent://rcs0aj94.relay.yinnho.cn -> (rcs0aj94, rcs0aj94.relay.yinnho.cn:8600)
    let url = url.strip_prefix("agent://")?;
    let parts: Vec<&str> = url.split('.').collect();
    if parts.len() >= 4 && parts[1] == "relay" {
        let id = parts[0].to_string();
        let addr = format!("{}:8600", url);
        Some((id, addr))
    } else {
        None
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .init();

    // 从命令行获取 agent URL
    let url = std::env::args()
        .nth(1)
        .expect("Usage: test_client_relay <agent_url>\n  Example: test_client_relay agent://rcs0aj94.relay.yinnho.cn");

    let (target, relay_addr) = parse_agent_url(&url)
        .ok_or_else(|| anyhow::anyhow!("Invalid agent URL: {}", url))?;

    tracing::info!("连接 Agent: {}", url);
    tracing::info!("Relay 地址: {}", relay_addr);

    // 连接 relay
    let stream = TcpStream::connect(&relay_addr).await?;
    tracing::info!("TCP 连接成功");

    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    // 发送连接请求
    let connect_msg = serde_json::json!({
        "type": "connect",
        "target": target
    });
    writer.write_all(format!("{}\n", connect_msg).as_bytes()).await?;
    writer.flush().await?;

    // 等待响应
    let mut response = String::new();
    reader.read_line(&mut response).await?;
    tracing::info!("Relay 响应: {}", response.trim());

    let resp: serde_json::Value = serde_json::from_str(&response)?;
    if resp.get("type").and_then(|t| t.as_str()) == Some("error") {
        tracing::error!("连接失败: {:?}", resp);
        return Ok(());
    }

    // 发送 JSON-RPC 请求
    let requests = vec![
        ("getServerInfo", serde_json::json!({})),
        ("listAgents", serde_json::json!({})),
        ("getAgentHelp", serde_json::json!({"agentId": "claude"})),
    ];

    for (method, params) in requests {
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": method,
            "method": method,
            "params": params
        });

        tracing::info!("发送请求: {}", method);
        writer.write_all(format!("{}\n", request).as_bytes()).await?;
        writer.flush().await?;

        // 读取响应（跳过心跳）
        let resp = loop {
            let mut response = String::new();
            reader.read_line(&mut response).await?;
            let resp: serde_json::Value = serde_json::from_str(&response)?;
            if resp.get("type").map(|t| t.as_str() == Some("ping") || t.as_str() == Some("pong")).unwrap_or(false) {
                continue;
            }
            break resp;
        };
        tracing::info!("响应: {}", serde_json::to_string_pretty(&resp)?);
    }

    tracing::info!("测试完成");
    Ok(())
}
