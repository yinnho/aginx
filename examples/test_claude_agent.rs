//! 测试 Claude Agent - 通过 relay 调用本地 Claude CLI

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 从命令行获取 target aginx ID
    let target = std::env::args().nth(1).expect("Usage: test_claude_agent <aginx_id>");
    let message = std::env::args().nth(2).unwrap_or_else(|| "你是谁？".to_string());
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
    let resp: serde_json::Value = serde_json::from_str(&response)?;

    if resp.get("type").and_then(|t| t.as_str()) == Some("error") {
        tracing::error!("连接失败: {:?}", resp);
        return Ok(());
    }

    tracing::info!("已连接到 Aginx: {}", target);

    // 发送 sendMessage 请求到 Claude agent
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": "1",
        "method": "sendMessage",
        "params": {
            "agentId": "claude",
            "message": message
        }
    });

    tracing::info!("发送消息给 Claude Agent: {}", message);
    writer.write_all(format!("{}\n", request).as_bytes()).await?;
    writer.flush().await?;

    // 读取响应（跳过心跳消息）
    let resp = loop {
        let mut response = String::new();
        reader.read_line(&mut response).await?;
        let resp: serde_json::Value = serde_json::from_str(&response)?;

        // 跳过心跳消息
        if resp.get("type").map(|t| t.as_str() == Some("ping") || t.as_str() == Some("pong")).unwrap_or(false) {
            tracing::trace!("跳过心跳消息: {:?}", resp);
            continue;
        }
        break resp;
    };

    tracing::info!("========================================");
    tracing::info!("原始响应: {}", serde_json::to_string_pretty(&resp)?);
    tracing::info!("========================================");

    if let Some(result) = resp.get("result") {
        if result.is_null() {
            tracing::error!("result 为 null");
        } else if let Some(response_text) = result.get("response") {
            println!("{}", response_text.as_str().unwrap_or(""));
        } else {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
    } else if let Some(error) = resp.get("error") {
        tracing::error!("错误: {:?}", error);
    } else {
        tracing::error!("响应中没有 result 或 error");
    }

    Ok(())
}
