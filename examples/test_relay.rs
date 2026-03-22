//! Test relay server connection

use futures::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

#[tokio::main]
async fn main() {
    let uri = std::env::var("RELAY_URL").unwrap_or_else(|_| "ws://127.0.0.1:8600".to_string());
    
    println!("Connecting to {}", uri);
    
    let (ws, _) = tokio_tungstenite::connect_async(&uri).await.unwrap();
    let (mut sink, mut stream) = ws.split();

    // 发送注册请求
    let register = serde_json::json!({"type": "register", "id": null});
    sink.send(Message::Text(register.to_string())).await.unwrap();
    println!("Sent: {}", register);

    // 接收响应
    let response = stream.next().await.unwrap().unwrap();
    println!("Received: {}", response);

    let msg: serde_json::Value = serde_json::from_str(&response.to_string()).unwrap();
    if msg["type"] == "registered" {
        println!("SUCCESS!");
        println!("  ID: {}", msg["id"]);
        println!("  URL: {}", msg["url"]);
    } else {
        println!("ERROR: Unexpected response");
    }
}
