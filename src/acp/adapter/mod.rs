//! Agent adapter — spawns CLI process per prompt
//!
//! The only adapter model: start process → stdin message → stdout chunks → result

use std::time::Duration;
use tokio::sync::mpsc;

use crate::agent::AgentInfo;

/// Prompt adapter — spawns a CLI process, streams stdout back
pub struct PromptAdapter {
    command: String,
    args_template: Vec<String>,
    env: std::collections::HashMap<String, String>,
    timeout_secs: u64,
    resume_args: Option<Vec<String>>,
}

impl PromptAdapter {
    pub fn new(agent_info: &AgentInfo) -> Self {
        Self {
            command: agent_info.command.clone(),
            args_template: agent_info.args.clone(),
            env: agent_info.env.clone(),
            timeout_secs: agent_info.timeout.unwrap_or(120),
            resume_args: agent_info.resume_args.clone(),
        }
    }

    /// Run a prompt: spawn CLI process, write message to stdin, stream stdout chunks.
    /// Returns (session_id, stop_reason) on success, or the process exits with error.
    /// When the `tx` receiver is dropped (client disconnect), the child process is killed.
    pub async fn prompt(
        &self,
        message: &str,
        session_id: Option<&str>,
        cwd: Option<&str>,
        tx: mpsc::Sender<String>,
    ) {
        let command = self.command.clone();
        let message = message.to_string();
        let timeout_secs = self.timeout_secs;
        let env = self.env.clone();
        let cwd = cwd
            .filter(|dir| !dir.is_empty())
            .and_then(|dir| {
                // Validate: must exist, be a directory, and be within home directory
                let path = std::path::Path::new(dir);
                let canonical = path.canonicalize().ok()?;
                let home = dirs::home_dir()?;
                let home_canonical = home.canonicalize().ok()?;
                if canonical.starts_with(&home_canonical) {
                    Some(canonical.to_string_lossy().to_string())
                } else {
                    None
                }
            });
        let session_id_owned = session_id.map(|s| s.to_string());

        // Build args: sanitize sessionId to prevent command injection
        let mut args: Vec<String> = self.args_template.iter()
            .map(|arg| {
                if let Some(sid) = session_id {
                    arg.replace("${SESSION_ID}", sid)
                } else {
                    arg.clone()
                }
            })
            .collect();

        if let (Some(ref resume_args), Some(sid)) = (&self.resume_args, session_id) {
            for arg in resume_args {
                args.push(arg.replace("${SESSION_ID}", sid));
            }
        }

        tokio::spawn(async move {
            let mut cmd = tokio::process::Command::new(&command);
            cmd.args(&args)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());

            if let Some(ref dir) = cwd {
                cmd.current_dir(dir);
            }

            for (k, v) in &env {
                cmd.env(k, v);
            }

            match cmd.spawn() {
                Ok(mut child) => {
                    // Write message to stdin — propagate errors
                    if let Some(mut stdin) = child.stdin.take() {
                        use tokio::io::AsyncWriteExt;
                        if let Err(e) = stdin.write_all(message.as_bytes()).await {
                            let _ = child.kill().await;
                            let _ = child.wait().await;
                            let err = serde_json::json!({
                                "jsonrpc": "2.0",
                                "error": {"code": -32603, "message": format!("Failed to write to agent stdin: {}", e)}
                            });
                            let _ = tx.send(serde_json::to_string(&err).unwrap()).await;
                            return;
                        }
                        if let Err(e) = stdin.write_all(b"\n").await {
                            let _ = child.kill().await;
                            let _ = child.wait().await;
                            let err = serde_json::json!({
                                "jsonrpc": "2.0",
                                "error": {"code": -32603, "message": format!("Failed to write to agent stdin: {}", e)}
                            });
                            let _ = tx.send(serde_json::to_string(&err).unwrap()).await;
                            return;
                        }
                        drop(stdin);
                    }

                    // Capture stderr in background
                    let stderr_handle = child.stderr.take().map(|stderr| {
                        tokio::spawn(async move {
                            use tokio::io::AsyncBufReadExt;
                            let reader = tokio::io::BufReader::new(stderr);
                            let mut lines = reader.lines();
                            let mut output = String::new();
                            while let Ok(Some(line)) = lines.next_line().await {
                                tracing::debug!("Agent stderr: {}", line);
                                output.push_str(&line);
                                output.push('\n');
                            }
                            output
                        })
                    });

                    // Track if client disconnected (tx.send failed)
                    let mut client_disconnected = false;

                    // Read stdout line by line, send as chunk notifications
                    let read_stdout = async {
                        if let Some(stdout) = child.stdout.take() {
                            use tokio::io::AsyncBufReadExt;
                            let reader = tokio::io::BufReader::new(stdout);
                            let mut lines = reader.lines();

                            while let Ok(Some(line)) = lines.next_line().await {
                                let mut params = serde_json::json!({"text": line});
                                if let Some(ref sid) = session_id_owned {
                                    params["sessionId"] = serde_json::json!(sid);
                                }
                                let notification = serde_json::json!({
                                    "jsonrpc": "2.0",
                                    "method": "chunk",
                                    "params": params
                                });
                                if tx.send(serde_json::to_string(&notification).unwrap_or_default()).await.is_err() {
                                    client_disconnected = true;
                                    break;
                                }
                            }
                        }
                    };

                    let timed_out = match tokio::time::timeout(
                        Duration::from_secs(timeout_secs),
                        read_stdout,
                    ).await {
                        Ok(_) => false,
                        Err(_) => {
                            tracing::warn!("Agent process timed out after {}s", timeout_secs);
                            let _ = child.kill().await;
                            true
                        }
                    };

                    // If client disconnected, kill the process immediately
                    if client_disconnected {
                        tracing::info!("Client disconnected, killing agent process");
                        let _ = child.kill().await;
                    }

                    let exit_status = child.wait().await.ok();
                    let stderr_output = if let Some(h) = stderr_handle {
                        h.await.unwrap_or_default()
                    } else {
                        String::new()
                    };

                    // Don't send response if client already disconnected
                    if client_disconnected {
                        return;
                    }

                    let code = exit_status.as_ref().and_then(|s| s.code()).unwrap_or(0);

                    if timed_out {
                        let err = serde_json::json!({
                            "jsonrpc": "2.0",
                            "error": {"code": -32603, "message": format!("Agent timed out after {}s", timeout_secs)}
                        });
                        let _ = tx.send(serde_json::to_string(&err).unwrap()).await;
                    } else if code != 0 {
                        let detail = if stderr_output.is_empty() {
                            format!("Agent exited with code {}", code)
                        } else {
                            format!("Agent exited {}: {}", code, stderr_output.trim())
                        };
                        let err = serde_json::json!({
                            "jsonrpc": "2.0",
                            "error": {"code": -32603, "message": detail}
                        });
                        let _ = tx.send(serde_json::to_string(&err).unwrap()).await;
                    } else {
                        // Success: send done signal with sessionId for client resume
                        let mut result = serde_json::json!({"stopReason": "endTurn"});
                        if let Some(ref sid) = session_id_owned {
                            result["sessionId"] = serde_json::json!(sid);
                        }
                        let done = serde_json::json!({
                            "jsonrpc": "2.0",
                            "result": result
                        });
                        let _ = tx.send(serde_json::to_string(&done).unwrap()).await;
                    }
                }
                Err(e) => {
                    let err = serde_json::json!({
                        "jsonrpc": "2.0",
                        "error": {"code": -32603, "message": format!("Failed to start agent: {}", e)}
                    });
                    let _ = tx.send(serde_json::to_string(&err).unwrap()).await;
                }
            }
        });
    }
}
