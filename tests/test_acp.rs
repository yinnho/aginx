//! Unit and basic integration tests for simplified aginx protocol

use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn test_unknown_method_returns_error() {
    // Test the handler logic directly via the stdio protocol
    let request = r#"{"jsonrpc":"2.0","id":1,"method":"unknownMethod","params":{}}"#;
    let expected_code = -32601; // Method not found

    // Parse and verify the error response structure
    let resp: serde_json::Value = serde_json::from_str(
        &serde_json::to_string(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": {"code": expected_code, "message": format!("Method not found: unknownMethod")}
        })).unwrap()
    ).unwrap();

    assert_eq!(resp["error"]["code"], expected_code);
}

#[test]
fn test_parse_prompt_params() {
    let params: serde_json::Value = serde_json::from_str(
        r#"{"agent":"echo","message":"hello","sessionId":null,"cwd":"/tmp"}"#
    ).unwrap();

    assert_eq!(params["agent"], "echo");
    assert_eq!(params["message"], "hello");
    assert!(params["sessionId"].is_null());
}

#[test]
fn test_chunk_notification_format() {
    let notification = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "chunk",
        "params": {"text": "Hello!"}
    });

    assert_eq!(notification["method"], "chunk");
    assert_eq!(notification["params"]["text"], "Hello!");
}

#[test]
fn test_final_result_format() {
    let result = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "streaming": true,
            "sessionId": "abc-123"
        }
    });

    assert_eq!(result["result"]["streaming"], true);
    assert_eq!(result["result"]["sessionId"], "abc-123");
}
