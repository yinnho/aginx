//! 测试客户端 - 连接到 agent:// URL (支持 TLS)
//!
//! Usage:
//!   cargo run --example test_client_relay agent://hcrqzbf6.relay.yinnho.cn
//!   cargo run --example test_client_relay agent://hcrqzbf6.relay.yinnho.cn --no-tls

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, AsyncRead, AsyncWrite, BufReader};
use tokio::net::TcpStream;

fn parse_agent_url(url: &str) -> Option<(String, String, String)> {
    // agent://hcrqzbf6.relay.yinnho.cn -> (hcrqzbf6, hcrqzbf6.relay.yinnho.cn, relay.yinnho.cn)
    let url = url.strip_prefix("agent://")?;
    let parts: Vec<&str> = url.split('.').collect();
    if parts.len() >= 4 && parts[1] == "relay" {
        let id = parts[0].to_string();
        let tls_domain = parts[1..].join("."); // relay.yinnho.cn (matches *.yinnho.cn)
        Some((id, url.to_string(), tls_domain))
    } else {
        None
    }
}

/// 读一行，跳过心跳消息
async fn read_response<R: AsyncBufReadExt + AsyncRead + Unpin>(reader: &mut R) -> anyhow::Result<serde_json::Value> {
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        let resp: serde_json::Value = serde_json::from_str(line.trim())?;
        let msg_type = resp.get("type").and_then(|t| t.as_str());
        if msg_type == Some("ping") || msg_type == Some("pong") {
            continue;
        }
        return Ok(resp);
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .init();

    let args: Vec<String> = std::env::args().collect();
    let no_tls = args.iter().any(|a| a == "--no-tls");

    let url = args.iter().find(|a| a.starts_with("agent://"))
        .ok_or_else(|| anyhow::anyhow!("Usage: test_client_relay <agent_url> [--no-tls]"))?
        .clone();

    let (target, _full_domain, tls_domain) = parse_agent_url(&url)
        .ok_or_else(|| anyhow::anyhow!("Invalid agent URL: {}", url))?;

    let port = if no_tls { 8600 } else { 8443 };
    let addr = format!("{}:{}", tls_domain, port);

    tracing::info!("连接 Agent: {}", url);
    tracing::info!("Relay 地址: {} (TLS: {})", addr, !no_tls);

    // TCP 连接
    let tcp_stream = TcpStream::connect(&addr).await?;
    tracing::info!("TCP 连接成功");

    // 根据参数决定是否 TLS
    if no_tls {
        run_test(tcp_stream, target).await
    } else {
        let tls_connector = native_tls::TlsConnector::new()?;
        let tls_stream = tokio_native_tls::TlsConnector::from(tls_connector)
            .connect(&tls_domain, tcp_stream)
            .await?;
        tracing::info!("TLS 握手成功");
        run_test(tls_stream, target).await
    }
}

async fn run_test<S: AsyncRead + AsyncWrite + Unpin>(stream: S, target: String) -> anyhow::Result<()> {
    let (reader, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(reader);

    // 发送连接请求
    let connect_msg = serde_json::json!({
        "type": "connect",
        "target": target
    });
    writer.write_all(format!("{}\n", connect_msg).as_bytes()).await?;
    writer.flush().await?;
    tracing::info!("发送连接请求: target={}", target);

    // 等待响应
    let resp = read_response(&mut reader).await?;
    tracing::info!("Relay 响应: {}", resp);

    if resp.get("type").and_then(|t| t.as_str()) == Some("error") {
        tracing::error!("连接失败: {:?}", resp);
        return Ok(());
    }

    tracing::info!("连接成功! 开始测试 ACP 协议...");

    // 测试 ACP 请求
    let requests = vec![
        ("initialize", serde_json::json!({
            "protocolVersion": "0.1.0",
            "clientInfo": {"name": "test-client", "version": "0.1.0"}
        })),
        ("listAgents", serde_json::json!({})),
        ("session/new", serde_json::json!({
            "cwd": "/tmp",
            "_meta": {"agentId": "claude"}
        })),
    ];

    let mut id_counter = 1u64;
    for (method, params) in requests {
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id_counter,
            "method": method,
            "params": params
        });

        tracing::info!("→ {}", method);
        writer.write_all(format!("{}\n", request).as_bytes()).await?;
        writer.flush().await?;

        let resp = read_response(&mut reader).await?;
        tracing::info!("← {}", serde_json::to_string_pretty(&resp)?);
        id_counter += 1;
    }

    tracing::info!("测试完成");
    Ok(())
}
