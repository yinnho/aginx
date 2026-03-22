//! Test client connection to aginx via relay

use futures::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

#[tokio::main]
async fn main() {
    let relay_url = std::env::var("RELAY_URL").unwrap_or_else(|_| "ws://127.0.0.1:8600".to_string());
    let target = std::env::var("TARGET_ID").expect("TARGET_ID required");

    println!("Connecting to relay: {}", relay_url);
    println!("Target Aginx: {}", target);

    let (ws, _) = tokio_tungstenite::connect_async(&relay_url).await.unwrap();
    let (mut sink, mut stream) = ws.split();

    // 发送连接请求
    let connect = serde_json::json!({"type": "connect", "target": target});
    sink.send(Message::Text(connect.to_string())).await.unwrap();
    println!("Sent: {}", connect);

    // 接收响应
    let response = stream.next().await.unwrap().unwrap();
    println!("Received: {}", response);

    let msg: serde_json::Value = serde_json::from_str(&response.to_string()).unwrap();
    if msg["type"] == "connected" {
        println!("Connected to Aginx [{}]!", target);

        // 发送 JSON-RPC 请求
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getServerInfo"
        });
        sink.send(Message::Text(request.to_string())).await.unwrap();
        println!("\nSent: {}", request);

        // 接收响应
        let response = stream.next().await.unwrap().unwrap();
        println!("Received: {}", response);

        let resp: serde_json::Value = serde_json::from_str(&response.to_string()).unwrap();
        if let Some(result) = resp.get("result") {
            println!("\nServer Info:");
            println!("  Version: {}", result["version"]);
            println!("  Agent Count: {}", result["agentCount"]);
        }
    } else {
        println!("ERROR: {}", msg);
    }
}
