//! 测试会话管理 - 完整流程测试
//!
//! Usage:
//!   cargo run --example test_session agent://rcs0aj94.relay.yinnho.cn

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

fn parse_agent_url(url: &str) -> Option<(String, String)> {
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

    let url = std::env::args()
        .nth(1)
        .expect("Usage: test_session <agent_url>\n  Example: test_session agent://rcs0aj94.relay.yinnho.cn");

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

    // 测试 1: 创建会话（使用 echo-session agent）
    tracing::info!("\n=== 测试 1: 创建会话 (echo-session) ===");
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": "1",
        "method": "createSession",
        "params": {
            "agentId": "echo-session"
        }
    });
    writer.write_all(format!("{}\n", request).as_bytes()).await?;
    writer.flush().await?;

    let resp = read_response(&mut reader).await?;
    tracing::info!("响应: {}", serde_json::to_string_pretty(&resp)?);

    let session_id = resp
        .get("result")
        .and_then(|r| r.get("sessionId"))
        .and_then(|s| s.as_str())
        .ok_or_else(|| anyhow::anyhow!("Failed to get sessionId"))?
        .to_string();

    tracing::info!("✅ 会话创建成功: {}", session_id);

    // 测试 2: 会话模式 - 第一条消息
    tracing::info!("\n=== 测试 2: 会话模式 - 第一条消息 ===");
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": "2",
        "method": "sendMessage",
        "params": {
            "sessionId": session_id,
            "message": "请记住我的名字叫 Sophie，后面我会问你。"
        }
    });
    writer.write_all(format!("{}\n", request).as_bytes()).await?;
    writer.flush().await?;

    let resp = read_response(&mut reader).await?;
    tracing::info!("响应: {}", serde_json::to_string_pretty(&resp)?);

    // 测试 3: 会话模式 - 第二条消息（测试上下文）
    tracing::info!("\n=== 测试 3: 会话模式 - 测试上下文保持 ===");
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": "3",
        "method": "sendMessage",
        "params": {
            "sessionId": session_id,
            "message": "你还记得我的名字吗？"
        }
    });
    writer.write_all(format!("{}\n", request).as_bytes()).await?;
    writer.flush().await?;

    let resp = read_response(&mut reader).await?;
    tracing::info!("响应: {}", serde_json::to_string_pretty(&resp)?);

    // 测试 4: 获取服务器信息（查看会话数量）
    tracing::info!("\n=== 测试 4: 获取服务器信息 ===");
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": "4",
        "method": "getServerInfo",
        "params": {}
    });
    writer.write_all(format!("{}\n", request).as_bytes()).await?;
    writer.flush().await?;

    let resp = read_response(&mut reader).await?;
    tracing::info!("响应: {}", serde_json::to_string_pretty(&resp)?);

    // 测试 5: 关闭会话
    tracing::info!("\n=== 测试 5: 关闭会话 ===");
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": "5",
        "method": "closeSession",
        "params": {
            "sessionId": session_id
        }
    });
    writer.write_all(format!("{}\n", request).as_bytes()).await?;
    writer.flush().await?;

    let resp = read_response(&mut reader).await?;
    tracing::info!("响应: {}", serde_json::to_string_pretty(&resp)?);

    tracing::info!("\n✅ 会话管理测试完成");
    Ok(())
}

async fn read_response(reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>) -> anyhow::Result<serde_json::Value> {
    loop {
        let mut response = String::new();
        reader.read_line(&mut response).await?;
        let resp: serde_json::Value = serde_json::from_str(&response)?;

        // 跳过心跳消息
        if resp.get("type").map(|t| t.as_str() == Some("ping") || t.as_str() == Some("pong")).unwrap_or(false) {
            continue;
        }

        return Ok(resp);
    }
}
