//! 简单 TCP 测试客户端 - 直连本地 aginx

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_target(false)
        .init();

    let addr = std::env::var("AGINX_ADDR").unwrap_or_else(|_| "127.0.0.1:86".to_string());

    println!("连接到 Aginx: {}", addr);
    let stream = TcpStream::connect(&addr).await?;
    println!("TCP 连接成功!");

    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    // 1. 发送 initialize
    let init_req = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "initialize",
        "params": {
            "protocolVersion": "1.0",
            "clientInfo": {"name": "test", "version": "1.0"}
        },
        "id": 1
    });
    println!("\n>>> 发送 initialize");
    writer.write_all(format!("{}\n", init_req).as_bytes()).await?;
    writer.flush().await?;

    let mut line = String::new();
    if reader.read_line(&mut line).await? > 0 {
        println!("<<< 收到: {}", line.trim());
    }

    // 2. 发送 session/new
    let session_req = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "session/new",
        "params": {
            "agentId": "claude",
            "cwd": "/Users/sophiehe/Documents/yinnhoos/aginx"
        },
        "id": 2
    });
    println!("\n>>> 发送 session/new");
    writer.write_all(format!("{}\n", session_req).as_bytes()).await?;
    writer.flush().await?;

    line.clear();
    if reader.read_line(&mut line).await? > 0 {
        println!("<<< 收到: {}", line.trim());
    }

    // 解析 sessionId
    let session_id = if let Ok(val) = serde_json::from_str::<serde_json::Value>(&line) {
        val.get("result")
            .and_then(|r| r.get("sessionId"))
            .and_then(|s| s.as_str())
            .map(|s| s.to_string())
    } else {
        None
    };

    if let Some(sid) = session_id {
        println!("\n获取到 sessionId: {}", sid);

        // 3. 第一次 prompt
        let prompt_req1 = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/prompt",
            "params": {
                "sessionId": sid,
                "prompt": [{"type": "text", "text": "记住: 我的名字是小明"}]
            },
            "id": 3
        });
        println!("\n>>> 第一次 prompt");
        writer.write_all(format!("{}\n", prompt_req1).as_bytes()).await?;
        writer.flush().await?;

        // 读取响应
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => { println!("连接关闭"); break; }
                Ok(_) => {
                    let text = line.trim();
                    if text.is_empty() { continue; }
                    if let Ok(val) = serde_json::from_str::<serde_json::Value>(text) {
                        if val.get("id").is_some() { break; }
                    }
                }
                Err(e) => { println!("读取错误: {}", e); break; }
            }
        }

        // 4. 第二次 prompt
        let prompt_req2 = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/prompt",
            "params": {
                "sessionId": sid,
                "prompt": [{"type": "text", "text": "我叫什么名字?"}]
            },
            "id": 4
        });
        println!("\n>>> 第二次 prompt");
        writer.write_all(format!("{}\n", prompt_req2).as_bytes()).await?;
        writer.flush().await?;

        // 读取响应
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => { println!("连接关闭"); break; }
                Ok(_) => {
                    let text = line.trim();
                    if text.is_empty() { continue; }
                    if let Ok(val) = serde_json::from_str::<serde_json::Value>(text) {
                        if val.get("id").is_some() { break; }
                    }
                }
                Err(e) => { println!("读取错误: {}", e); break; }
            }
        }

        // 5. 测试 getConversationMessages
        let list_req = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "listConversations",
            "params": {
                "agentId": "claude"
            },
            "id": 5
        });
        println!("\n>>> 发送 listConversations");
        writer.write_all(format!("{}\n", list_req).as_bytes()).await?;
        writer.flush().await?;

        line.clear();
        if reader.read_line(&mut line).await? > 0 {
            println!("<<< 收到: {}", line.trim());
        }

        // 6. 获取会话消息
        let msg_req = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "getConversationMessages",
            "params": {
                "sessionId": sid
            },
            "id": 6
        });
        println!("\n>>> 发送 getConversationMessages for {}", sid);
        writer.write_all(format!("{}\n", msg_req).as_bytes()).await?;
        writer.flush().await?;

        line.clear();
        if reader.read_line(&mut line).await? > 0 {
            println!("<<< 收到: {}", line.trim());
        }
    }

    println!("\n测试完成!");
    Ok(())
}
