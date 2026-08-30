//! Agent adapter — spawns CLI process per prompt
//!
//! The only adapter model: start process → stdin message → stdout chunks → result

use std::time::Duration;
use tokio::sync::mpsc;

use crate::agent::AgentInfo;

pub mod translate;
use translate::OutputFormat;

/// Prompt adapter — spawns a CLI process, streams stdout back
pub struct PromptAdapter {
    command: String,
    args_template: Vec<String>,
    env: std::collections::HashMap<String, String>,
    timeout_secs: u64,
    resume_args: Option<Vec<String>>,
    /// stdout dialect（接入包 output 声明驱动，网关核心零 CLI 知识）
    output: OutputFormat,
    /// 注册项绑定的默认文件夹：客户端不带 cwd 时 spawn 在这（会话锚定点）
    working_dir: Option<String>,
    /// 注册名（记账归属）
    agent_id: String,
    /// 会话台账：成功轮结束后以收割的真 sessionId 记账（§2.4.1 事实源）
    ledger: crate::agent::ledger::SessionLedger,
}

impl PromptAdapter {
    pub fn new(agent_info: &AgentInfo, ledger: crate::agent::ledger::SessionLedger) -> Self {
        Self {
            command: agent_info.command.clone(),
            args_template: agent_info.args.clone(),
            env: agent_info.env.clone(),
            timeout_secs: agent_info.timeout.unwrap_or(120),
            resume_args: agent_info.resume_args.clone(),
            output: OutputFormat::parse(agent_info.output.as_deref()),
            working_dir: agent_info.working_dir.clone(),
            agent_id: agent_info.id.clone(),
            ledger,
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
        let output = self.output;
        let agent_id = self.agent_id.clone();
        let ledger = self.ledger.clone();
        // cwd 解析优先级：客户端传入 > 注册项默认文件夹（working_dir）。
        // 两者都过同一道门：必须存在、是目录、在 home 内。
        let cwd = cwd
            .filter(|dir| !dir.is_empty())
            .or(self.working_dir.as_deref())
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

                    // 方言翻译收割状态（§2.5/§2.6）：chunk 只发翻译后的纯文本，
                    // 真 sessionId/成本字段从 result 行收割
                    let mut harvested: translate::TranslatedLine = Default::default();

                    // Read stdout line by line, translate dialect → chunk notifications
                    let read_stdout = async {
                        if let Some(stdout) = child.stdout.take() {
                            use tokio::io::AsyncBufReadExt;
                            let reader = tokio::io::BufReader::new(stdout);
                            let mut lines = reader.lines();

                            // 通道探���：子进程静默期（brain 思考）没有输出可
                            // 触发 send，靠周期 tick 检查接收端是否已断——
                            // 客户端跑路（relay disconnected → notify_task 退出
                            // → rx drop）时 ≤1s 内发现并杀进程，不留幽灵轮。
                            let mut chan_poll =
                                tokio::time::interval(std::time::Duration::from_secs(1));
                            chan_poll
                                .set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                            loop {
                                let line = tokio::select! {
                                    l = lines.next_line() => match l {
                                        Ok(Some(l)) => l,
                                        Ok(None) => break, // 子进程输出结束
                                        Err(_) => break,
                                    },
                                    _ = chan_poll.tick() => {
                                        if tx.is_closed() {
                                            client_disconnected = true;
                                            break;
                                        }
                                        continue;
                                    }
                                };
                                let t = translate::translate_line(output, &line);
                                if t.session_id.is_some() {
                                    harvested.session_id = t.session_id.clone();
                                }
                                if t.cost_usd.is_some() {
                                    harvested.cost_usd = t.cost_usd;
                                }
                                if t.duration_ms.is_some() {
                                    harvested.duration_ms = t.duration_ms;
                                }
                                if t.num_turns.is_some() {
                                    harvested.num_turns = t.num_turns;
                                }
                                harvested.is_error |= t.is_error;
                                if harvested.error_text.is_none() {
                                    harvested.error_text = t.error_text.clone();
                                }
                                for text in &t.chunks {
                                    let mut params = serde_json::json!({"text": text});
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
                                if client_disconnected {
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
                    } else if harvested.is_error {
                        // agent 自报失败（如 claude result.is_error）→ error 帧（§2.8）
                        let detail = harvested.error_text.clone()
                            .unwrap_or_else(|| format!("Agent exited with code {}", code));
                        let err = serde_json::json!({
                            "jsonrpc": "2.0",
                            "error": {"code": -32603, "message": detail}
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
                        // Success: sessionId = 翻译器收割的 agent 真会话 id，
                        // 无翻译器方言回显客户端传入值（§2.5 立法语义）
                        let mut result = serde_json::json!({"stopReason": "endTurn"});
                        let final_sid = harvested.session_id.clone().or_else(|| session_id_owned.clone());
                        if let Some(ref sid) = final_sid {
                            result["sessionId"] = serde_json::json!(sid);
                        }
                        if let Some(cost) = harvested.cost_usd {
                            result["costUsd"] = serde_json::json!(cost);
                        }
                        if let Some(ms) = harvested.duration_ms {
                            result["durationMs"] = serde_json::json!(ms);
                        }
                        if let Some(n) = harvested.num_turns {
                            result["numTurns"] = serde_json::json!(n);
                        }
                        let done = serde_json::json!({
                            "jsonrpc": "2.0",
                            "result": result
                        });
                        let _ = tx.send(serde_json::to_string(&done).unwrap()).await;
                        // 成功轮记账：只记翻译器收割到的真会话 id（raw 方言不记）
                        if let Some(ref sid) = final_sid {
                            ledger.record_turn(&agent_id, sid, &message);
                        }
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

    /// 借用轮直通：prompt 带 `sessionTicket` → 对 agent 进程跑一次完整 ACP 会话
    /// （spawn → initialize → session/new → session/prompt 带 ticket/materials），
    /// agent_message_chunk 翻译成 `chunk` 通知转发，最终 result 原样带回
    /// `sessionTicket`/`files`。借用轮本身无状态（会话真源在票据里），所以
    /// 每次 spawn 一次性进程即可，无需持久进程管理。
    #[allow(clippy::too_many_arguments)]
    pub async fn prompt_borrowed(
        &self,
        message: &str,
        session_ticket: serde_json::Value,
        materials: Option<serde_json::Value>,
        active_flow: Option<String>,
        borrower: Option<String>,
        cwd: Option<&str>,
        tx: mpsc::Sender<String>,
    ) -> Result<(), String> {
        use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};

        let command = self.command.clone();
        let args = self.args_template.clone();
        let env = self.env.clone();
        let timeout_secs = self.timeout_secs.clamp(600, 3600);
        let cwd = cwd
            .filter(|dir| !dir.is_empty())
            .and_then(|dir| {
                // Same validation as prompt(): must be a directory under home.
                let path = std::path::Path::new(dir);
                let canonical = path.canonicalize().ok()?;
                let home = dirs::home_dir()?.canonicalize().ok()?;
                canonical.starts_with(&home).then(|| canonical.to_string_lossy().to_string())
            });
        let message = message.to_string();
        let active_flow = active_flow.unwrap_or_default();
        let borrower = borrower.unwrap_or_default();

        tokio::spawn(async move {
            let run = async {
                let mut cmd = tokio::process::Command::new(&command);
                // 任何退出路径（超时取消、Err 提前返回、借用方断连）都杀进程，
                // 不留幽灵——成功路径的显式 kill 变成幂等收尾。
                cmd.kill_on_drop(true);
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
                let mut child = cmd
                    .spawn()
                    .map_err(|e| format!("Failed to start agent: {e}"))?;
                let mut stdin =
                    child.stdin.take().ok_or("agent stdin unavailable")?;
                let stdout =
                    child.stdout.take().ok_or("agent stdout unavailable")?;
                // Drain stderr so the bridge never blocks on a full pipe (tracing goes there).
                let stderr = child.stderr.take();
                let stderr_task = stderr.map(|s| {
                    tokio::spawn(async move {
                        let mut reader = tokio::io::BufReader::new(s);
                        let mut line = String::new();
                        loop {
                            line.clear();
                            match reader.read_line(&mut line).await {
                                Ok(0) | Err(_) => break,
                                Ok(_) => {}
                            }
                        }
                    })
                });

                let mut reader = tokio::io::BufReader::new(stdout);

                // request → wait for the response line with matching id (skip notifications).
                macro_rules! rpc {
                    ($id:expr, $method:expr, $params:expr) => {{
                        let req = serde_json::json!({
                            "jsonrpc": "2.0", "id": $id, "method": $method, "params": $params,
                        });
                        stdin
                            .write_all(serde_json::to_string(&req).unwrap().as_bytes())
                            .await
                            .map_err(|e| format!("agent stdin write failed: {e}"))?;
                        stdin.write_all(b"\n").await.ok();
                        stdin.flush().await.ok();
                        loop {
                            let mut line = String::new();
                            let n = reader.read_line(&mut line).await
                                .map_err(|e| format!("agent stdout read failed: {e}"))?;
                            if n == 0 {
                                return Err("agent closed stdout".to_string());
                            }
                            let v: serde_json::Value = match serde_json::from_str(line.trim()) {
                                Ok(v) => v,
                                Err(_) => continue,
                            };
                            if v.get("id").is_some() && v.get("method").is_none() {
                                break v;
                            }
                        }
                    }};
                }

                let init = rpc!(1, "initialize", serde_json::json!({"protocolVersion": 1}));
                if let Some(err) = init.get("error") {
                    return Err(format!("agent initialize failed: {err}"));
                }

                let new_sess =
                    rpc!(2, "session/new", serde_json::json!({}));
                let sid = new_sess
                    .pointer("/result/sessionId")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| format!("session/new failed: {new_sess}"))?
                    .to_string();

                // session/prompt — notifications from here are turn content.
                let mut params = serde_json::json!({
                    "sessionId": sid,
                    "prompt": [{"type": "text", "text": message}],
                    "sessionTicket": session_ticket,
                });
                if let Some(m) = materials {
                    params["materials"] = m;
                }
                if !active_flow.is_empty() {
                    params["activeFlow"] = serde_json::json!(active_flow);
                }
                if !borrower.is_empty() {
                    params["borrower"] = serde_json::json!(borrower);
                }
                let req = serde_json::json!({
                    "jsonrpc": "2.0", "id": 3, "method": "session/prompt", "params": params,
                });
                stdin
                    .write_all(serde_json::to_string(&req).unwrap().as_bytes())
                    .await
                    .map_err(|e| format!("agent stdin write failed: {e}"))?;
                stdin.write_all(b"\n").await.ok();
                stdin.flush().await.ok();
                // NOTE: do NOT drop stdin here — the bridge's reader thread treats
                // stdin EOF as shutdown and exits before the prompt response is
                // written. Keep it open; the child is killed after we read the
                // final response (or on timeout).

                let mut final_resp: Option<serde_json::Value> = None;
                // 通道探活：agent 静默期（brain 思考）无输出可触发 send，靠
                // 周期 tick 查接收端——借用方断连 ≤1s 内退出（kill_on_drop
                // 收尸），不留幽灵轮。
                let mut chan_poll =
                    tokio::time::interval(std::time::Duration::from_secs(1));
                chan_poll
                    .set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    let mut line = String::new();
                    let n = tokio::select! {
                        n = reader.read_line(&mut line) => {
                            n.map_err(|e| format!("agent stdout read failed: {e}"))?
                        }
                        _ = chan_poll.tick() => {
                            if tx.is_closed() {
                                return Err("client disconnected".to_string());
                            }
                            continue;
                        }
                    };
                    if n == 0 {
                        break;
                    }
                    let v: serde_json::Value = match serde_json::from_str(line.trim()) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    if v.get("method").and_then(|m| m.as_str()) == Some("session/update") {
                        // Translate to the gateway's chunk notification shape.
                        let text = v
                            .pointer("/params/update/content/text")
                            .and_then(|t| t.as_str())
                            .unwrap_or("");
                        if !text.is_empty() {
                            let note = serde_json::json!({
                                "jsonrpc": "2.0", "method": "chunk",
                                "params": {"text": text, "sessionId": sid},
                            });
                            if tx.send(serde_json::to_string(&note).unwrap_or_default()).await.is_err()
                            {
                                return Err("client disconnected".to_string());
                            }
                        }
                    } else if v.get("id") == Some(&serde_json::json!(3)) {
                        final_resp = Some(v);
                        break;
                    }
                }

                let resp = final_resp.ok_or("agent closed stdout before prompt response")?;
                if let Some(err) = resp.get("error") {
                    return Err(format!(
                        "borrowed turn failed: {}",
                        err.get("message").and_then(|m| m.as_str()).unwrap_or("unknown")
                    ));
                }
                // Done reading — close stdin and reap the child so it doesn't linger.
                drop(stdin);
                let _ = child.kill().await;
                let _ = child.wait().await;                let result = resp.get("result").cloned().unwrap_or(serde_json::json!({}));
                // stopReason 词汇归一：agent 侧说 ACP（end_turn），外部协议说
                // endTurn（与 prompt() 成功路径一致）——网关是翻译边界。
                let stop = match result.get("stopReason").and_then(|v| v.as_str()) {
                    Some("end_turn") => "endTurn".to_string(),
                    Some(other) => other.to_string(),
                    None => "endTurn".to_string(),
                };
                let mut out = serde_json::json!({
                    "stopReason": stop,
                    "streaming": true,
                    "sessionId": sid,
                });
                if let Some(t) = result.get("sessionTicket") {
                    out["sessionTicket"] = t.clone();
                }
                if let Some(f) = result.get("files") {
                    out["files"] = f.clone();
                }
                if let Some(t) = stderr_task {
                    let _ = t.await;
                }
                Ok(out)
            };

            match tokio::time::timeout(Duration::from_secs(timeout_secs), run).await {
                Ok(Ok(result)) => {
                    let done = serde_json::json!({"jsonrpc": "2.0", "result": result});
                    let _ = tx.send(serde_json::to_string(&done).unwrap_or_default()).await;
                }
                Ok(Err(e)) => {
                    let err = serde_json::json!({
                        "jsonrpc": "2.0",
                        "error": {"code": -32603, "message": e},
                    });
                    let _ = tx.send(serde_json::to_string(&err).unwrap_or_default()).await;
                }
                Err(_) => {
                    let err = serde_json::json!({
                        "jsonrpc": "2.0",
                        "error": {"code": -32603, "message": format!("borrowed turn timed out after {}s", timeout_secs)},
                    });
                    let _ = tx.send(serde_json::to_string(&err).unwrap_or_default()).await;
                }
            }
        });
        Ok(())
    }
}
