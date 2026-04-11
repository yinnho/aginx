//! Test Claude JSON output with actual tool calls
//!
//! This test sends a prompt that triggers file operations and verifies:
//! 1. Tool use events are detected
//! 2. Permission requests are generated
//! 3. Tool results are parsed

use std::io::Write;
use std::sync::mpsc;

#[tokio::main]
async fn main() {
    println!("========================================");
    println!("Testing Claude JSON with Tool Calls");
    println!("========================================\n");

    // This test requires Claude CLI to be installed and configured
    // It will create a temporary file in /tmp

    let test_prompt = "Create a test file at /tmp/aginx_test_123.txt with content 'Hello from aginx test'";

    println!("Prompt: {}", test_prompt);
    println!("\nExpected behavior:");
    println!("  1. Claude should output text response");
    println!("  2. Claude should use Write tool");
    println!("  3. aginx should detect tool_use and request permission");
    println!("  4. aginx should parse tool_result after permission");
    println!("\nNote: This test requires Claude CLI with ANTHROPIC_API_KEY set\n");

    // Check if ANTHROPIC_API_KEY is set
    if std::env::var("ANTHROPIC_API_KEY").is_err() {
        println!("⚠ ANTHROPIC_API_KEY not set. Skipping actual Claude test.");
        println!("  Set it with: export ANTHROPIC_API_KEY=your_key");
        return;
    }

    // Run test
    test_with_claude(test_prompt).await;
}

async fn test_with_claude(prompt: &str) {
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::thread;

    let aginx_path = "./target/release/aginx";

    // Start aginx
    let mut child = match Command::new(aginx_path)
        .args(&["acp", "--stdio"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            println!("✗ Failed to start aginx: {}", e);
            return;
        }
    };

    let stdin = child.stdin.take().expect("Failed to get stdin");
    let stdout = child.stdout.take().expect("Failed to get stdout");
    let stderr = child.stderr.take().expect("Failed to get stderr");

    // Start stdout reader
    let (tx, rx) = mpsc::channel::<String>();
    thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            if let Ok(line) = line {
                tx.send(line).ok();
            }
        }
    });

    // Start stderr reader (for logs)
    thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines() {
            if let Ok(line) = line {
                eprintln!("[aginx] {}", line);
            }
        }
    });

    let mut stdin = stdin;

    // Send initialize
    println!("[1/4] Sending initialize...");
    send_request(&mut stdin, 1, "initialize", serde_json::json!({
        "protocolVersion": "0.15.0",
        "clientInfo": {"name": "test", "version": "1.0.0"}
    }));

    let resp = wait_for_response(&rx, std::time::Duration::from_secs(5));
    assert!(resp.is_some(), "Initialize failed");
    println!("  ✓ Initialized");

    // Send newSession
    println!("[2/4] Creating session...");
    send_request(&mut stdin, 2, "newSession", serde_json::json!({
        "cwd": "/tmp",
        "_meta": {"agentId": "claude"}
    }));

    let resp = wait_for_response(&rx, std::time::Duration::from_secs(5));
    let session_id = extract_session_id(&resp.expect("No session response"));
    println!("  ✓ Session created: {}", session_id);

    // Send prompt (this will trigger tool use)
    println!("[3/4] Sending prompt (will trigger Write tool)...");
    send_request(&mut stdin, 3, "prompt", serde_json::json!({
        "sessionId": session_id,
        "prompt": [{"type": "text", "text": prompt}]
    }));

    // Collect events
    let start = std::time::Instant::now();
    let mut text_received = false;
    let mut tool_use_received = false;
    let mut permission_received = false;
    let mut tool_result_received = false;

    while start.elapsed() < std::time::Duration::from_secs(60) {
        match rx.try_recv() {
            Ok(line) => {
                if line.trim().is_empty() {
                    continue;
                }

                // Parse as JSON
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&line) {
                    // Check for notification
                    if let Some(method) = json.get("method").and_then(|m| m.as_str()) {
                        match method {
                            "sessionUpdate" => {
                                if let Some(params) = json.get("params") {
                                    if let Some(update) = params.get("update") {
                                        if let Some(update_type) = update.get("sessionUpdate").and_then(|u| u.as_str()) {
                                            match update_type {
                                                "agent_message_chunk" => {
                                                    if !text_received {
                                                        println!("  ✓ Received text chunk");
                                                        text_received = true;
                                                    }
                                                }
                                                "tool_call" => {
                                                    if let Some(title) = update.get("title").and_then(|t| t.as_str()) {
                                                        if !tool_use_received {
                                                            println!("  ✓ Tool call: {}", title);
                                                            tool_use_received = true;
                                                        }
                                                    }
                                                }
                                                "tool_call_update" => {
                                                    if let Some(status) = update.get("status").and_then(|s| s.as_str()) {
                                                        if !tool_result_received {
                                                            println!("  ✓ Tool result: {}", status);
                                                            tool_result_received = true;
                                                        }
                                                    }
                                                }
                                                _ => {}
                                            }
                                        }
                                    }
                                }
                            }
                            "requestPermission" => {
                                if !permission_received {
                                    println!("  ✓ Permission request received!");
                                    if let Some(params) = json.get("params") {
                                        if let Some(desc) = params.get("description").and_then(|d| d.as_str()) {
                                            println!("    Description: {}", desc);
                                        }
                                    }
                                    permission_received = true;

                                    // Auto-allow for testing
                                    println!("  → Auto-allowing permission...");
                                    send_request(&mut stdin, 4, "permissionResponse", serde_json::json!({
                                        "sessionId": session_id,
                                        "outcome": {"outcome": "selected", "optionId": "1"}
                                    }));
                                }
                            }
                            _ => {}
                        }
                    }

                    // Check for prompt response
                    if let Some(id) = json.get("id").and_then(|i| i.as_i64()) {
                        if id == 3 {
                            if let Some(result) = json.get("result") {
                                if let Some(stop_reason) = result.get("stopReason").and_then(|s| s.as_str()) {
                                    println!("  ✓ Prompt completed with stop_reason: {}", stop_reason);
                                    break;
                                }
                            }
                        }
                    }
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(_) => break,
        }
    }

    // Summary
    println!("\n[4/4] Test Summary:");
    println!("  Text chunks: {}", if text_received { "✓" } else { "✗" });
    println!("  Tool use: {}", if tool_use_received { "✓" } else { "✗" });
    println!("  Permission request: {}", if permission_received { "✓" } else { "✗" });
    println!("  Tool result: {}", if tool_result_received { "✓" } else { "✗" });

    // Cleanup
    let _ = child.kill();

    // Verify file was created
    let test_file = "/tmp/aginx_test_123.txt";
    if std::path::Path::new(test_file).exists() {
        println!("  ✓ Test file created successfully!");
        if let Ok(content) = std::fs::read_to_string(test_file) {
            println!("    Content: {}", content.trim());
        }
        // Clean up
        let _ = std::fs::remove_file(test_file);
    } else {
        println!("  ✗ Test file not created");
    }

    if text_received && tool_use_received && permission_received {
        println!("\n✓ All tests passed!");
    } else {
        println!("\n✗ Some tests failed");
    }
}

fn send_request(stdin: &mut std::process::ChildStdin, id: i64, method: &str, params: serde_json::Value) {
    let req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params
    });
    writeln!(stdin, "{}", req.to_string()).ok();
    stdin.flush().ok();
}

fn wait_for_response(rx: &mpsc::Receiver<String>, timeout: std::time::Duration) -> Option<String> {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        match rx.try_recv() {
            Ok(line) => return Some(line),
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(_) => return None,
        }
    }
    None
}

fn extract_session_id(response: &str) -> String {
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(response) {
        if let Some(result) = json.get("result") {
            if let Some(id) = result.get("sessionId").and_then(|s| s.as_str()) {
                return id.to_string();
            }
        }
    }
    "unknown".to_string()
}
