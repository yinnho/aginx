//! 测试客户端 - 连接到 relay 并与 aginx 通信

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 从命令行获取 target aginx ID
    let target = std::env::args().nth(1).expect("Usage: test_client_relay <aginx_id>");
    let relay_addr = format!("{}.relay.yinnho.cn:8600", target);

    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_target(false)
        .init();

    tracing::info!("连接 Relay: {}", relay_addr);

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

        // 读取响应
        let mut response = String::new();
        reader.read_line(&mut response).await?;
        let resp: serde_json::Value = serde_json::from_str(&response)?;
        tracing::info!("响应: {}", serde_json::to_string_pretty(&resp)?);
    }

    tracing::info!("测试完成");
    Ok(())
}
