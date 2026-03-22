//! Full test: aginx + client via relay

use futures::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

#[tokio::main]
async fn main() {
    let relay_url = std::env::var("RELAY_URL").unwrap_or_else(|_| "wss://relay.yinnho.cn".to_string());

    println!("=== 启动 Aginx (服务端) ===");

    // 1. Aginx 连接并注册
    let (aginx_ws, _) = tokio_tungstenite::connect_async(&relay_url).await.unwrap();
    let (mut aginx_sink, mut aginx_stream) = aginx_ws.split();

    let register = serde_json::json!({"type": "register", "id": null});
    aginx_sink.send(Message::Text(register.to_string())).await.unwrap();
    println!("Sent: {}", register);

    let response = aginx_stream.next().await.unwrap().unwrap();
    let msg: serde_json::Value = serde_json::from_str(&response.to_string()).unwrap();
    let aginx_id = msg["id"].as_str().unwrap().to_string();
    let aginx_url = msg["url"].as_str().unwrap().to_string();
    println!("Aginx 注册成功!");
    println!("  ID: {}", aginx_id);
    println!("  URL: {}", aginx_url);

    // 2. 客户端连接到 Aginx
    println!("\n=== 启动 Client (客户端) ===");

    let (client_ws, _) = tokio_tungstenite::connect_async(&relay_url).await.unwrap();
    let (mut client_sink, mut client_stream) = client_ws.split();

    let connect = serde_json::json!({"type": "connect", "target": aginx_id});
    client_sink.send(Message::Text(connect.to_string())).await.unwrap();
    println!("Sent: {}", connect);

    let response = client_stream.next().await.unwrap().unwrap();
    let msg: serde_json::Value = serde_json::from_str(&response.to_string()).unwrap();
    println!("Received: {}", msg);

    if msg["type"] == "connected" {
        println!("Client 连接成功!");

        // 3. 客户端发送 JSON-RPC 请求
        println!("\n=== 发送 JSON-RPC 请求 ===");

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getServerInfo"
        });
        client_sink.send(Message::Text(request.to_string())).await.unwrap();
        println!("Client 发送: {}", request);

        // 4. Aginx 接收请求并处理
        let aginx_msg = aginx_stream.next().await.unwrap().unwrap();
        println!("Aginx 收到: {}", aginx_msg);

        // 解析并响应
        let data: serde_json::Value = serde_json::from_str(&aginx_msg.to_string()).unwrap();
        if data["type"] == "data" {
            let client_id = data["client_id"].as_str().unwrap();
            let rpc_request = &data["data"];

            println!("Aginx 处理请求: method={}", rpc_request["method"]);

            // 构造响应
            let response = serde_json::json!({
                "clientId": client_id,
                "jsonrpc": "2.0",
                "id": rpc_request["id"],
                "result": {
                    "version": "0.1.0",
                    "agentCount": 2,
                    "relayUrl": aginx_url
                }
            });
            aginx_sink.send(Message::Text(response.to_string())).await.unwrap();
            println!("Aginx 发送响应");
        }

        // 5. 客户端接收响应
        let client_response = client_stream.next().await.unwrap().unwrap();
        println!("Client 收到: {}", client_response);

        let resp: serde_json::Value = serde_json::from_str(&client_response.to_string()).unwrap();
        if let Some(result) = resp.get("result") {
            println!("\n=== 测试成功! ===");
            println!("Server Version: {}", result["version"]);
            println!("Agent Count: {}", result["agentCount"]);
            println!("Relay URL: {}", result["relayUrl"]);
        }
    } else {
        println!("ERROR: {}", msg);
    }
}
