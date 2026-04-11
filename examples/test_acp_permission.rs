//! ACP 权限流程自动化测试
//!
//! 测试方案C的完整权限处理流程:
//! 1. 初始化
//! 2. 创建 Session
//! 3. 发送 Prompt (触发工具调用)
//! 4. 接收权限请求通知
//! 5. 发送权限响应
//! 6. 验证流式输出
//!
//! 运行方式:
//!   cargo run --example test_acp_permission

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

#[derive(Debug, serde::Deserialize)]
struct AcpResponse {
    jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "error")]
    error_value: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<serde_json::Value>,
}

fn main() {
    println!("========================================");
    println!("ACP 权限流程测试");
    println!("========================================\n");

    // 启动 aginx ACP 进程
    let mut child = Command::new("./target/release/aginx")
        .args(&["acp", "--stdio"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("启动 aginx 失败，请确保已构建: cargo build --release");

    let stdin = child.stdin.take().expect("获取 stdin 失败");
    let stdout = child.stdout.take().expect("获取 stdout 失败");
    let stderr = child.stderr.take().expect("获取 stderr 失败");

    // 启动 stderr 读取线程（用于查看日志）
    thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines() {
            if let Ok(line) = line {
                eprintln!("[AGINX LOG] {}", line);
            }
        }
    });

    // 启动 stdout 读取线程
    let (tx, rx) = mpsc::channel::<String>();
    thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            if let Ok(line) = line {
                if !line.trim().is_empty() {
                    tx.send(line).expect("发送失败");
                }
            }
        }
    });

    let mut stdin = stdin;

    // 测试 1: 初始化
    println!("[测试 1] 初始化...");
    let init_request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "0.15.0",
            "clientInfo": {
                "name": "test-client",
                "version": "1.0.0"
            }
        }
    });
    send_request(&mut stdin, &init_request);

    let response = wait_for_response(&rx, Duration::from_secs(5));
    assert!(response.is_some(), "初始化超时");
    let resp: AcpResponse = serde_json::from_str(&response.unwrap()).expect("解析失败");
    assert!(resp.result.is_some(), "初始化失败");
    println!("✓ 初始化成功\n");

    // 测试 2: 创建 Session
    println!("[测试 2] 创建 Session...");
    let new_session_request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "newSession",
        "params": {
            "cwd": ".",
            "_meta": {
                "agentId": "claude"
            }
        }
    });
    send_request(&mut stdin, &new_session_request);

    let response = wait_for_response(&rx, Duration::from_secs(5));
    assert!(response.is_some(), "创建 Session 超时");
    let resp: AcpResponse = serde_json::from_str(&response.unwrap()).expect("解析失败");
    let session_id = resp
        .result
        .as_ref()
        .and_then(|r| r.get("sessionId"))
        .and_then(|s| s.as_str())
        .expect("获取 sessionId 失败")
        .to_string();
    println!("✓ Session 创建成功: {}\n", session_id);

    // 测试 3: 发送 Prompt（触发 Edit 工具，需要权限）
    println!("[测试 3] 发送 Prompt (触发 Edit 权限)...");
    let prompt_request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "prompt",
        "params": {
            "sessionId": session_id,
            "prompt": [
                {
                    "type": "text",
                    "text": "创建一个测试文件 test.txt，内容为 Hello World"
                }
            ]
        }
    });
    send_request(&mut stdin, &prompt_request);

    // 等待接收通知和响应
    let mut permission_received = false;
    let mut permission_request_id = String::new();
    let start = std::time::Instant::now();

    while start.elapsed() < Duration::from_secs(30) {
        match rx.try_recv() {
            Ok(line) => {
                println!("收到: {}", line);

                if let Ok(resp) = serde_json::from_str::<AcpResponse>(&line) {
                    // 检查是否是 requestPermission 通知
                    if resp.method.as_deref() == Some("requestPermission") {
                        permission_received = true;
                        if let Some(params) = resp.params {
                            if let Some(req_id) = params.get("requestId").and_then(|v| v.as_str()) {
                                permission_request_id = req_id.to_string();
                                println!("✓ 收到权限请求: {}", permission_request_id);

                                // 显示工具信息
                                if let Some(tool_call) = params.get("toolCall") {
                                    if let Some(title) = tool_call.get("title").and_then(|v| v.as_str()) {
                                        println!("  工具: {}", title);
                                    }
                                }

                                // 显示选项
                                if let Some(options) = params.get("options").and_then(|v| v.as_array()) {
                                    println!("  选项:");
                                    for (i, opt) in options.iter().enumerate() {
                                        let label = opt.get("label").and_then(|v| v.as_str()).unwrap_or("unknown");
                                        let kind = opt.get("kind").and_then(|v| v.as_str()).unwrap_or("unknown");
                                        println!("    {}. {} ({})", i + 1, label, kind);
                                    }
                                }
                            }
                        }
                        break;
                    }
                }
            }
            Err(mpsc::TryRecvError::Empty) => {
                thread::sleep(Duration::from_millis(100));
            }
            Err(_) => break,
        }
    }

    assert!(permission_received, "未收到权限请求通知");
    println!("✓ 权限请求通知接收成功\n");

    // 测试 4: 发送权限响应
    println!("[测试 4] 发送权限响应 (选择允许)...");
    let permission_response = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "permissionResponse",
        "params": {
            "sessionId": session_id,
            "outcome": {
                "outcome": "selected",
                "optionId": "1"
            }
        }
    });
    send_request(&mut stdin, &permission_response);

    // 等待最终响应
    let start = std::time::Instant::now();
    let mut completed = false;

    while start.elapsed() < Duration::from_secs(30) {
        match rx.try_recv() {
            Ok(line) => {
                println!("收到: {}", line);

                if let Ok(resp) = serde_json::from_str::<AcpResponse>(&line) {
                    if let Some(id) = resp.id {
                        if id == 4 && resp.result.is_some() {
                            completed = true;
                            println!("✓ 权限响应成功");
                            break;
                        }
                    }

                    // 检查是否是 sessionUpdate 通知
                    if resp.method.as_deref() == Some("sessionUpdate") {
                        if let Some(params) = resp.params {
                            if let Some(update) = params.get("update") {
                                if let Some(update_type) = update.get("sessionUpdate").and_then(|v| v.as_str()) {
                                    match update_type {
                                        "agent_message_chunk" => {
                                            if let Some(content) = update.get("content") {
                                                if let Some(text) = content.get("text").and_then(|v| v.as_str()) {
                                                    print!("[输出] {}", text);
                                                }
                                            }
                                        }
                                        "tool_call" => {
                                            if let Some(title) = update.get("title").and_then(|v| v.as_str()) {
                                                println!("[工具调用] {}", title);
                                            }
                                        }
                                        "tool_call_update" => {
                                            if let Some(status) = update.get("status").and_then(|v| v.as_str()) {
                                                println!("[工具状态] {}", status);
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Err(mpsc::TryRecvError::Empty) => {
                thread::sleep(Duration::from_millis(100));
            }
            Err(_) => break,
        }
    }

    assert!(completed, "等待最终响应超时");
    println!("\n✓ 测试完成!");

    // 清理
    let _ = child.kill();

    println!("\n========================================");
    println!("所有测试通过!");
    println!("========================================");
}

fn send_request(stdin: &mut std::process::ChildStdin, request: &serde_json::Value) {
    let json = request.to_string();
    writeln!(stdin, "{}", json).expect("发送请求失败");
    stdin.flush().expect("刷新失败");
    println!("发送: {}", json);
}

fn wait_for_response(rx: &mpsc::Receiver<String>, timeout: Duration) -> Option<String> {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        match rx.try_recv() {
            Ok(line) => return Some(line),
            Err(mpsc::TryRecvError::Empty) => {
                thread::sleep(Duration::from_millis(100));
            }
            Err(_) => return None,
        }
    }
    None
}
