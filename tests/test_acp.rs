//! ACP stdio mode integration test
//!
//! Run with: cargo test --manifest-path /Users/sophiehe/Documents/yinnhoos/aginx/Cargo.toml test_acp

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn get_aginx_bin() -> PathBuf {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .unwrap_or_else(|_| ".".to_string());
    PathBuf::from(manifest_dir).join("target/debug/aginx")
}

fn run_acp_request(requests: &[&str]) -> Vec<String> {
    let aginx_bin = get_aginx_bin();

    let mut child = Command::new(&aginx_bin)
        .args(["acp", "--stdio"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("Failed to start aginx");

    let stdin = child.stdin.as_mut().expect("Failed to get stdin");
    for req in requests {
        writeln!(stdin, "{}", req).expect("Failed to write request");
    }
    let _ = stdin; // release stdin to signal EOF

    let output = child.wait_with_output().expect("Failed to wait for child");
    let stdout = String::from_utf8_lossy(&output.stdout);

    stdout
        .lines()
        .filter(|line| !line.is_empty())
        .map(|s| s.to_string())
        .collect()
}

#[test]
fn test_acp_initialize() {
    let requests = vec![
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"0.15.0","clientCapabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}"#,
    ];

    let responses = run_acp_request(&requests);
    assert!(!responses.is_empty(), "Should get a response");

    let response: serde_json::Value =
        serde_json::from_str(&responses[0]).expect("Response should be valid JSON");

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 1);
    assert!(response["result"].is_object());
    assert_eq!(response["result"]["protocolVersion"], "0.15.0");
    assert!(response["result"]["agentInfo"]["name"].is_string());
}

#[test]
#[ignore] // requires local claude agent configured
fn test_acp_new_session() {
    let requests = vec![
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"0.15.0","clientCapabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"newSession","params":{"cwd":"/tmp","_meta":{"agentId":"claude"}}}"#,
    ];

    let responses = run_acp_request(&requests);
    assert!(responses.len() >= 2, "Should get at least 2 responses");

    // Check the newSession response (second response)
    let response: serde_json::Value =
        serde_json::from_str(&responses[1]).expect("Response should be valid JSON");

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 2);
    assert!(response["result"]["sessionId"].is_string(), "Should have sessionId in result");
    println!("Created session: {}", response["result"]["sessionId"]);
}

#[test]
fn test_acp_list_sessions() {
    let requests = vec![
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"0.15.0","clientCapabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"listSessions","params":{}}"#,
    ];

    let responses = run_acp_request(&requests);
    assert!(responses.len() >= 2, "Should get at least 2 responses");

    let response: serde_json::Value =
        serde_json::from_str(&responses[1]).expect("Response should be valid JSON");

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 2);
    assert!(response["result"]["sessions"].is_array());
}

#[test]
fn test_acp_unknown_method() {
    let requests = vec![
        r#"{"jsonrpc":"2.0","id":1,"method":"unknownMethod","params":{}}"#,
    ];

    let responses = run_acp_request(&requests);
    assert!(!responses.is_empty(), "Should get a response");

    let response: serde_json::Value =
        serde_json::from_str(&responses[0]).expect("Response should be valid JSON");

    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 1);
    assert!(response["error"].is_object());
    assert_eq!(response["error"]["code"], -32601); // Method not found
}

#[test]
fn test_acp_invalid_json() {
    let requests = vec![r#"not valid json"#];

    let responses = run_acp_request(&requests);
    assert!(!responses.is_empty(), "Should get a response");

    let response: serde_json::Value =
        serde_json::from_str(&responses[0]).expect("Response should be valid JSON");

    assert!(response["error"].is_object());
    assert_eq!(response["error"]["code"], -32700); // Parse error
}
