//! 通用配置驱动的会话文件扫描
//!
//! 根据 aginx.toml 中的 `storage_path` + `storage_format` 配置扫描会话文件。
//! 支持: "copilot-events", "gemini-json", "claude-jsonl"

use std::path::Path;

/// 根据配置扫描会话文件
pub fn scan_sessions(
    storage_path: &str,
    storage_format: &str,
    agent_id: &str,
) -> Vec<serde_json::Value> {
    let expanded = shellexpand::tilde(storage_path).to_string();
    let dir = Path::new(&expanded);
    if !dir.exists() {
        return Vec::new();
    }

    match storage_format {
        "copilot-events" => scan_copilot(dir, agent_id),
        "gemini-json" => scan_gemini(dir, agent_id),
        "claude-jsonl" => scan_claude_jsonl(dir, agent_id),
        _ => {
            tracing::warn!("Unknown storage_format: {}", storage_format);
            Vec::new()
        }
    }
}

/// 根据配置读取会话消息
pub fn read_messages(
    session_id: &str,
    limit: usize,
    storage_path: &str,
    storage_format: &str,
) -> Vec<serde_json::Value> {
    let expanded = shellexpand::tilde(storage_path).to_string();
    let dir = Path::new(&expanded);
    if !dir.exists() {
        return Vec::new();
    }

    match storage_format {
        "copilot-events" => read_copilot_messages(session_id, limit, dir),
        "gemini-json" => read_gemini_messages(session_id, limit, dir),
        _ => Vec::new(),
    }
}

/// Copilot: session-state/{id}/workspace.yaml + events.jsonl
fn scan_copilot(storage_dir: &Path, agent_id: &str) -> Vec<serde_json::Value> {
    let mut sessions = Vec::new();

    if let Ok(entries) = std::fs::read_dir(storage_dir) {
        for entry in entries.flatten() {
            let session_dir = entry.path();
            if !session_dir.is_dir() { continue; }

            let workspace_file = session_dir.join("workspace.yaml");
            if !workspace_file.exists() { continue; }

            if let Ok(content) = std::fs::read_to_string(&workspace_file) {
                let mut session_id = None;
                let mut cwd = None;
                let mut summary = None;
                let mut created_at = 0u64;
                let mut updated_at = 0u64;

                for line in content.lines() {
                    if let Some((key, value)) = line.split_once(':') {
                        let key = key.trim();
                        let value = value.trim();
                        match key {
                            "id" => session_id = Some(value.to_string()),
                            "cwd" => cwd = Some(value.to_string()),
                            "summary" => summary = Some(value.to_string()),
                            "created_at" => {
                                created_at = parse_iso_timestamp(value).unwrap_or(0);
                            }
                            "updated_at" => {
                                updated_at = parse_iso_timestamp(value).unwrap_or(0);
                            }
                            _ => {}
                        }
                    }
                }

                if let Some(id) = session_id {
                    sessions.push(serde_json::json!({
                        "sessionId": id,
                        "agentId": agent_id,
                        "title": summary.unwrap_or_else(|| "Copilot Session".to_string()),
                        "lastMessage": null,
                        "workdir": cwd,
                        "createdAt": created_at,
                        "updatedAt": updated_at
                    }));
                }
            }
        }
    }

    sessions.sort_by(|a, b| {
        let a_updated = a.get("updatedAt").and_then(|v| v.as_u64()).unwrap_or(0);
        let b_updated = b.get("updatedAt").and_then(|v| v.as_u64()).unwrap_or(0);
        b_updated.cmp(&a_updated)
    });
    sessions
}

/// Gemini: {project}/chats/*.json
fn scan_gemini(storage_dir: &Path, agent_id: &str) -> Vec<serde_json::Value> {
    let mut sessions = Vec::new();

    if let Ok(entries) = std::fs::read_dir(storage_dir) {
        for entry in entries.flatten() {
            let project_dir = entry.path();
            if !project_dir.is_dir() { continue; }

            let chats_dir = project_dir.join("chats");
            if !chats_dir.exists() || !chats_dir.is_dir() { continue; }

            if let Ok(chat_entries) = std::fs::read_dir(&chats_dir) {
                for chat_entry in chat_entries.flatten() {
                    let path = chat_entry.path();
                    if path.extension().and_then(|e| e.to_str()) != Some("json") { continue; }

                    let content = match std::fs::read_to_string(&path) {
                        Ok(c) => c,
                        Err(_) => continue,
                    };

                    let session: serde_json::Value = match serde_json::from_str(&content) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };

                    let session_id = session.get("sessionId")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if session_id.is_empty() { continue; }

                    let start_time = session.get("startTime")
                        .and_then(|v| v.as_str())
                        .and_then(|ts| parse_iso_timestamp(ts))
                        .unwrap_or(0);

                    let last_updated = session.get("lastUpdated")
                        .and_then(|v| v.as_str())
                        .and_then(|ts| parse_iso_timestamp(ts))
                        .unwrap_or(start_time);

                    let title = session.get("messages")
                        .and_then(|m| m.as_array())
                        .and_then(|msgs| msgs.iter().find(|msg| {
                            msg.get("type").and_then(|t| t.as_str()) == Some("user")
                        }))
                        .and_then(|msg| msg.get("content"))
                        .and_then(|c| c.as_array())
                        .and_then(|arr| arr.first())
                        .and_then(|item| item.get("text"))
                        .and_then(|t| t.as_str())
                        .map(|s| s.chars().take(100).collect::<String>());

                    let workdir_name = project_dir.file_name().and_then(|n| n.to_str()).unwrap_or("");

                    sessions.push(serde_json::json!({
                        "sessionId": session_id,
                        "agentId": agent_id,
                        "title": title,
                        "lastMessage": null,
                        "workdir": format!("/{}", workdir_name),
                        "createdAt": start_time,
                        "updatedAt": last_updated
                    }));
                }
            }
        }
    }

    sessions.sort_by(|a, b| {
        let a_updated = a.get("updatedAt").and_then(|v| v.as_u64()).unwrap_or(0);
        let b_updated = b.get("updatedAt").and_then(|v| v.as_u64()).unwrap_or(0);
        b_updated.cmp(&a_updated)
    });
    sessions
}

/// Claude JSONL: {project}/*.jsonl
fn scan_claude_jsonl(storage_dir: &Path, agent_id: &str) -> Vec<serde_json::Value> {
    let mut results = Vec::new();
    if let Ok(entries) = std::fs::read_dir(storage_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                scan_claude_project_dir(&path, &mut results, agent_id);
            }
        }
    }
    results.sort_by(|a, b| {
        let a_updated = a.get("updatedAt").and_then(|v| v.as_u64()).unwrap_or(0);
        let b_updated = b.get("updatedAt").and_then(|v| v.as_u64()).unwrap_or(0);
        b_updated.cmp(&a_updated)
    });
    results
}

fn scan_claude_project_dir(
    dir: &Path,
    results: &mut Vec<serde_json::Value>,
    agent_id: &str,
) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") { continue; }
            if path.to_str().map(|s| s.contains("/subagents/")).unwrap_or(false) { continue; }

            let session_id = match path.file_stem().and_then(|s| s.to_str()) {
                Some(id) => id.to_string(),
                None => continue,
            };

            // Lightweight metadata extraction (first 30 lines)
            let file = match std::fs::File::open(&path) {
                Ok(f) => f,
                Err(_) => continue,
            };
            let reader = std::io::BufReader::new(file);

            let mut first_user_message: Option<String> = None;
            let mut last_assistant_text: Option<String> = None;
            let mut first_timestamp: Option<u64> = None;
            let mut cwd: Option<String> = None;

            for (i, line) in std::io::BufRead::lines(reader).enumerate() {
                if i >= 30 { break; }
                let line = match line { Ok(l) => l, Err(_) => break };
                let line = line.trim();
                if line.is_empty() { continue; }

                if let Ok(event) = serde_json::from_str::<serde_json::Value>(line) {
                    match event.get("type").and_then(|t| t.as_str()) {
                        Some("user") => {
                            if cwd.is_none() {
                                cwd = event.get("cwd").and_then(|c| c.as_str()).map(String::from);
                            }
                            if first_user_message.is_none() {
                                if let Some(text) = event.get("message")
                                    .and_then(|m| m.get("content"))
                                    .and_then(|c| c.as_str())
                                {
                                    if !text.contains("<local-command-caveat>")
                                        && !text.contains("<command-name>")
                                        && !text.contains("This session is being continued")
                                    {
                                        first_user_message = Some(text.chars().take(100).collect());
                                    }
                                }
                            }
                            if first_timestamp.is_none() {
                                first_timestamp = event.get("timestamp")
                                    .and_then(|t| t.as_str())
                                    .and_then(|ts| parse_iso_timestamp(ts));
                            }
                        }
                        Some("assistant") => {
                            if let Some(text) = event.get("message")
                                .and_then(|m| m.get("content"))
                                .and_then(|c| c.as_array())
                                .and_then(|blocks| blocks.iter().rev().find(|b| {
                                    b.get("type").and_then(|t| t.as_str()) == Some("text")
                                        && b.get("text").and_then(|t| t.as_str()).map(|s| !s.is_empty()).unwrap_or(false)
                                }))
                                .and_then(|b| b.get("text").and_then(|t| t.as_str()))
                            {
                                last_assistant_text = Some(text.chars().take(100).collect());
                            }
                        }
                        _ => {}
                    }
                }
            }

            let file_modified = std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .map(|t| t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64)
                .unwrap_or(0);

            let project_path = cwd.unwrap_or_default();
            let title = first_user_message.or_else(|| project_path.split('/').last().map(String::from));

            results.push(serde_json::json!({
                "sessionId": session_id,
                "agentId": agent_id,
                "title": title,
                "lastMessage": last_assistant_text,
                "workdir": project_path,
                "createdAt": first_timestamp.unwrap_or(file_modified),
                "updatedAt": file_modified
            }));
        }
    }
}

pub fn parse_iso_timestamp(ts: &str) -> Option<u64> {
    let ts = ts.trim_end_matches('Z').trim_end_matches('z');
    let parts: Vec<&str> = ts.split(|c: char| c == 'T' || c == ':' || c == '.' || c == '-').collect();
    if parts.len() < 6 { return None; }

    let year: i32 = parts[0].parse().ok()?;
    let month: u32 = parts[1].parse().ok()?;
    let day: u32 = parts[2].parse().ok()?;
    let hour: u32 = parts[3].parse().ok()?;
    let minute: u32 = parts[4].parse().ok()?;
    let second: u32 = parts[5].parse().unwrap_or(0);

    let mut days: i64 = 0;
    for y in 1970..year {
        days += if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 { 366 } else { 365 };
    }
    let days_in_months = [0, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    for m in 1..month {
        days += days_in_months[m as usize] as i64;
        if m == 2 && ((year % 4 == 0 && year % 100 != 0) || year % 400 == 0) {
            days += 1;
        }
    }
    days += (day - 1) as i64;
    if days < 0 { return None; }

    let secs = (days as u64 * 86400) + (hour as u64 * 3600) + (minute as u64 * 60) + (second as u64);
    Some(secs * 1000)
}

/// Read messages from Copilot events.jsonl
fn read_copilot_messages(session_id: &str, limit: usize, storage_dir: &Path) -> Vec<serde_json::Value> {
    let events_file = storage_dir.join(session_id).join("events.jsonl");
    if !events_file.exists() { return Vec::new(); }

    let file = match std::fs::File::open(&events_file) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let reader = std::io::BufReader::new(file);
    let mut messages = Vec::new();

    for line in std::io::BufRead::lines(reader) {
        let line = match line { Ok(l) => l, Err(_) => break };
        let line = line.trim();
        if line.is_empty() { continue; }

        let event: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        match event.get("type").and_then(|v| v.as_str()).unwrap_or("") {
            "user.message" => {
                if let Some(content) = event.get("data").and_then(|d| d.get("content")).and_then(|v| v.as_str()) {
                    if !content.is_empty() {
                        messages.push(serde_json::json!({ "role": "user", "content": content }));
                    }
                }
            }
            "assistant.message" => {
                if let Some(data) = event.get("data").and_then(|d| d.as_object()) {
                    let content = data.get("content").and_then(|v| v.as_str());
                    let encrypted = data.get("encryptedContent").and_then(|v| v.as_str());
                    let text = match content {
                        Some(s) if !s.is_empty() => s.to_string(),
                        _ => match encrypted {
                            Some(_) => "[encrypted content]".to_string(),
                            None => continue,
                        }
                    };
                    messages.push(serde_json::json!({ "role": "assistant", "content": text }));
                }
            }
            _ => {}
        }
    }

    let start = if messages.len() > limit { messages.len() - limit } else { 0 };
    messages[start..].to_vec()
}

/// Read messages from Gemini session JSON file
fn read_gemini_messages(session_id: &str, limit: usize, storage_dir: &Path) -> Vec<serde_json::Value> {
    // Find the session file by searching project directories
    let session_file = find_gemini_session_file(session_id, storage_dir);
    let path = match session_file {
        Some(p) => p,
        None => return Vec::new(),
    };

    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let session: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    let mut messages = Vec::new();
    if let Some(msgs) = session.get("messages").and_then(|m| m.as_array()) {
        for msg in msgs {
            let role = match msg.get("type").and_then(|t| t.as_str()).unwrap_or("unknown") {
                "user" => "user",
                "assistant" | "model" | "gemini" => "assistant",
                _ => continue,
            };
            let content_val = msg.get("content");
            let text = if let Some(arr) = content_val.and_then(|c| c.as_array()) {
                arr.iter()
                    .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n")
            } else if let Some(s) = content_val.and_then(|c| c.as_str()) {
                s.to_string()
            } else {
                continue;
            };
            if text.is_empty() { continue; }
            messages.push(serde_json::json!({ "role": role, "content": text }));
        }
    }

    let start = if messages.len() > limit { messages.len() - limit } else { 0 };
    messages[start..].to_vec()
}

/// Find Gemini session file by searching project directories
fn find_gemini_session_file(session_id: &str, storage_dir: &Path) -> Option<std::path::PathBuf> {
    if let Ok(entries) = std::fs::read_dir(storage_dir) {
        for entry in entries.flatten() {
            let project_dir = entry.path();
            if !project_dir.is_dir() { continue; }
            let chats_dir = project_dir.join("chats");
            if !chats_dir.exists() { continue; }

            if let Ok(chat_entries) = std::fs::read_dir(&chats_dir) {
                for chat_entry in chat_entries.flatten() {
                    let path = chat_entry.path();
                    if path.extension().and_then(|e| e.to_str()) != Some("json") { continue; }
                    let filename = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    if filename.contains(&session_id[..8.min(session_id.len())]) {
                        if let Ok(content) = std::fs::read_to_string(&path) {
                            if content.contains(session_id) {
                                return Some(path);
                            }
                        }
                    }
                }
            }
        }
    }
    None
}
