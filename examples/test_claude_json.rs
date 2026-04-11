//! Test Claude JSON output parsing
//!
//! This test verifies that aginx correctly parses Claude CLI's --output-format json output

use std::io::Write;
use std::process::{Command, Stdio};

fn main() {
    println!("========================================");
    println!("Testing Claude JSON Output Parsing");
    println!("========================================\n");

    // Test 1: Simulate Claude JSON events directly
    println!("[Test 1] Simulated Claude JSON Events");
    test_simulated_events();

    // Test 2: Test with actual aginx ACP mode (if Claude is available)
    println!("\n[Test 2] Integration Test with aginx");
    test_aginx_integration();
}

fn test_simulated_events() {
    // Simulate the JSON events Claude CLI would output
    let events = vec![
        r#"{"type": "text", "text": "I'll help you create a test file."}"#,
        r#"{"type": "tool_use", "id": "tool_01ABC", "name": "Write", "input": {"file_path": "/tmp/test.txt", "content": "Hello World"}}"#,
        r#"{"type": "tool_result", "tool_use_id": "tool_01ABC", "content": "File written successfully", "is_error": false}"#,
        r#"{"type": "text", "text": "Done! I've created the file."}"#,
        r#"{"type": "stop", "stop_reason": "end_turn"}"#,
    ];

    for event in &events {
        match serde_json::from_str::<ClaudeEvent>(event) {
            Ok(e) => println!("  ✓ Parsed: {:?}", e),
            Err(err) => println!("  ✗ Failed to parse: {}", err),
        }
    }
}

fn test_aginx_integration() {
    // Check if aginx binary exists
    let aginx_path = "./target/release/aginx";
    if !std::path::Path::new(aginx_path).exists() {
        println!("  ⚠ aginx not found at {}", aginx_path);
        println!("  Run: cargo build --release");
        return;
    }

    // Start aginx ACP mode
    let mut child = match Command::new(aginx_path)
        .args(&["acp", "--stdio"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            println!("  ✗ Failed to start aginx: {}", e);
            return;
        }
    };

    let stdin = child.stdin.as_mut().expect("Failed to get stdin");

    // Send initialize request
    let init_req = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"0.15.0","clientInfo":{"name":"test","version":"1.0.0"}}}"#;
    writeln!(stdin, "{}", init_req).expect("Failed to write");

    // Send newSession request
    let session_req = r#"{"jsonrpc":"2.0","id":2,"method":"newSession","params":{"cwd":"/tmp","_meta":{"agentId":"claude"}}}"#;
    writeln!(stdin, "{}", session_req).expect("Failed to write");

    // Flush and close stdin
    stdin.flush().expect("Failed to flush");
    drop(stdin);

    // Read output
    let output = child.wait_with_output().expect("Failed to read output");
    let stdout = String::from_utf8_lossy(&output.stdout);

    println!("  Responses:");
    for line in stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<serde_json::Value>(line) {
            Ok(json) => {
                if let Some(id) = json.get("id") {
                    println!("    ✓ Response id={}: {}", id, line.chars().take(100).collect::<String>());
                } else if json.get("method").is_some() {
                    println!("    ✓ Notification: {}", json.get("method").unwrap());
                }
            }
            Err(_) => println!("    ! Non-JSON: {}", line.chars().take(50).collect::<String>()),
        }
    }

    println!("  Exit code: {}", output.status.code().unwrap_or(-1));
}

#[derive(Debug, serde::Deserialize)]
#[serde(tag = "type")]
enum ClaudeEvent {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
        #[serde(rename = "_meta")]
        meta: Option<serde_json::Value>,
        #[serde(rename = "rawInput")]
        raw_input: Option<serde_json::Value>,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        #[serde(rename = "tool_use_id")]
        tool_use_id: String,
        content: Option<serde_json::Value>,
        #[serde(rename = "is_error")]
        is_error: Option<bool>,
    },
    #[serde(rename = "stop")]
    Stop {
        #[serde(rename = "stop_reason")]
        stop_reason: String,
    },
}
