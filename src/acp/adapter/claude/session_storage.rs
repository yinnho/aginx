//! Claude-specific session storage — JSONL file scanning/reading
//!
//! These functions parse Claude CLI's private JSONL session format.
//! This is NOT part of the standard ACP protocol — it's adapter-local.

use std::path::PathBuf;

/// Discovered session from Claude JSONL file scan
#[derive(Debug, Clone)]
struct DiscoveredSession {
    agent_session_id: String,
    project_path: String,
    first_user_message: Option<String>,
    last_assistant_text: Option<String>,
    file_modified: u64,
    first_timestamp: Option<u64>,
}

/// Find the JSONL file path in a directory tree by searching project directories
fn find_jsonl_path(agent_session_id: &str, projects_dir: &std::path::Path) -> Option<PathBuf> {
    let filename = format!("{}.jsonl", agent_session_id);

    if let Ok(entries) = std::fs::read_dir(projects_dir) {
        for entry in entries.flatten() {
            let project_dir = entry.path();
            if !project_dir.is_dir() { continue; }
            let candidate = project_dir.join(&filename);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }

    None
}

/// Parse metadata from a Claude JSONL file (lightweight - only read first ~30 lines)
fn parse_jsonl_metadata(
    path: &std::path::Path,
    agent_session_id: &str,
) -> Option<DiscoveredSession> {
    let file = std::fs::File::open(path).ok()?;
    let reader = std::io::BufReader::new(file);

    let mut first_user_message: Option<String> = None;
    let mut last_assistant_text: Option<String> = None;
    let mut first_timestamp: Option<u64> = None;
    let mut cwd: Option<String> = None;

    for (i, line) in std::io::BufRead::lines(reader).enumerate() {
        if i >= 30 { break; }
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let line = line.trim();
        if line.is_empty() { continue; }

        if let Ok(event) = serde_json::from_str::<serde_json::Value>(line) {
            let event_type = event.get("type").and_then(|t| t.as_str());

            match event_type {
                Some("queue-operation") => {
                    if first_user_message.is_none() {
                        if let Some(content) = event.get("content").and_then(|c| c.as_str()) {
                            let truncated: String = content.chars().take(100).collect();
                            first_user_message = Some(truncated);
                        }
                    }
                    if first_timestamp.is_none() {
                        first_timestamp = event.get("timestamp")
                            .and_then(|t| t.as_str())
                            .and_then(|ts| parse_iso_timestamp(ts));
                    }
                }
                Some("user") => {
                    if cwd.is_none() {
                        cwd = event.get("cwd").and_then(|c| c.as_str()).map(String::from);
                    }
                    if first_user_message.is_none() {
                        if let Some(msg) = event.get("message") {
                            let content_val = msg.get("content");
                            if let Some(text) = content_val.and_then(|c| c.as_str()) {
                                if !text.contains("<local-command-caveat>")
                                    && !text.contains("<command-name>")
                                    && !text.contains("This session is being continued")
                                    && !text.contains("<local-command-stdout>")
                                {
                                    let truncated: String = text.chars().take(100).collect();
                                    first_user_message = Some(truncated);
                                }
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
                    if let Some(msg) = event.get("message") {
                        if let Some(content_blocks) = msg.get("content").and_then(|c| c.as_array()) {
                            for block in content_blocks.iter().rev() {
                                if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                                    if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                                        if !text.is_empty() {
                                            let truncated: String = text.chars().take(100).collect();
                                            last_assistant_text = Some(truncated);
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    let file_modified = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .map(|t| t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64)
        .unwrap_or(0);

    let project_path = cwd.unwrap_or_default();

    Some(DiscoveredSession {
        agent_session_id: agent_session_id.to_string(),
        project_path,
        first_user_message,
        last_assistant_text,
        file_modified,
        first_timestamp,
    })
}

/// Scan a single project directory for Claude JSONL files
fn scan_claude_jsonl_dir(
    dir: &std::path::Path,
    results: &mut Vec<serde_json::Value>,
    agent_id: &str,
) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            // Skip subagent files
            if path.to_str().map(|s| s.contains("/subagents/")).unwrap_or(false) {
                continue;
            }

            let agent_session_id = match path.file_stem().and_then(|s| s.to_str()) {
                Some(id) => id.to_string(),
                None => continue,
            };

            if let Some(session) = parse_jsonl_metadata(&path, &agent_session_id) {
                let file_modified = std::fs::metadata(&path)
                    .and_then(|m| m.modified())
                    .map(|t| t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64)
                    .unwrap_or(0);

                let title = session.first_user_message.clone().or_else(|| {
                    session.project_path.split('/').last().map(String::from)
                });

                results.push(serde_json::json!({
                    "sessionId": agent_session_id,
                    "agentId": agent_id,
                    "title": title,
                    "lastMessage": session.last_assistant_text,
                    "workdir": session.project_path,
                    "createdAt": session.first_timestamp.unwrap_or(file_modified),
                    "updatedAt": file_modified
                }));
            }
        }
    }
}

/// Scan storage directory for Claude-style JSONL sessions
pub fn scan_claude_sessions(storage_dir: &std::path::Path, agent_id: &str) -> Vec<serde_json::Value> {
    let mut results = Vec::new();
    if let Ok(entries) = std::fs::read_dir(storage_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                scan_claude_jsonl_dir(&path, &mut results, agent_id);
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

/// Read messages from a Claude JSONL file, with limit and system message filtering.
/// Returns the last `limit` real conversation messages.
pub fn read_jsonl_messages_limited(
    agent_session_id: &str,
    limit: usize,
    storage_path: Option<&str>,
) -> Vec<serde_json::Value> {
    let storage_dir = match storage_path {
        Some(path) => {
            let expanded = shellexpand::tilde(path).to_string();
            std::path::PathBuf::from(expanded)
        }
        None => return Vec::new(),
    };

    let path = match find_jsonl_path(agent_session_id, &storage_dir) {
        Some(p) => p,
        None => {
            tracing::warn!("Claude JSONL not found for session {}", agent_session_id);
            return Vec::new();
        }
    };

    let file = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!("Failed to read Claude JSONL: {}", e);
            return Vec::new();
        }
    };

    let reader = std::io::BufReader::new(file);
    let mut all_messages: Vec<serde_json::Value> = Vec::new();
    let mut pending_tools: std::collections::HashMap<String, (String, String)> = std::collections::HashMap::new();

    for line in std::io::BufRead::lines(reader) {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let line = line.trim();
        if line.is_empty() { continue; }

        if let Ok(event) = serde_json::from_str::<serde_json::Value>(line) {
            let event_type = event.get("type").and_then(|t| t.as_str());

            match event_type {
                Some("user") => {
                    if let Some(msg) = event.get("message") {
                        let content_val = msg.get("content");
                        if let Some(text) = content_val.and_then(|c| c.as_str()) {
                            if !is_system_message(text) {
                                all_messages.push(serde_json::json!({
                                    "role": "user",
                                    "content": text
                                }));
                            }
                        } else if let Some(blocks) = content_val.and_then(|c| c.as_array()) {
                            for block in blocks {
                                match block.get("type").and_then(|t| t.as_str()) {
                                    Some("tool_result") => {
                                        let tool_use_id = block.get("tool_use_id")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("");
                                        let result_content = block.get("content")
                                            .and_then(|c| c.as_str())
                                            .unwrap_or("");
                                        let tool_label = pending_tools.remove(tool_use_id)
                                            .map(|(name, args)| format!("{}({})", name, args))
                                            .unwrap_or_else(|| "tool".to_string());
                                        let display = if result_content.len() > 2000 {
                                            let cut = result_content.char_indices()
                                                .take_while(|(i, _)| *i < 2000)
                                                .last()
                                                .map(|(i, c)| i + c.len_utf8())
                                                .unwrap_or(0);
                                            format!("> **{}**\n\n{}...\n\n*(result truncated)*", tool_label, &result_content[..cut])
                                        } else if result_content.is_empty() {
                                            format!("> **{}**\n\n*(empty result)*", tool_label)
                                        } else {
                                            format!("> **{}**\n\n{}", tool_label, result_content)
                                        };
                                        all_messages.push(serde_json::json!({
                                            "role": "tool",
                                            "content": display
                                        }));
                                    }
                                    Some("text") => {
                                        if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                                            if !text.is_empty() && !is_system_message(text) {
                                                all_messages.push(serde_json::json!({
                                                    "role": "user",
                                                    "content": text
                                                }));
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
                Some("assistant") => {
                    if let Some(msg) = event.get("message") {
                        if let Some(content_blocks) = msg.get("content").and_then(|c| c.as_array()) {
                            for block in content_blocks {
                                match block.get("type").and_then(|t| t.as_str()) {
                                    Some("text") => {
                                        if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                                            if !text.is_empty() {
                                                all_messages.push(serde_json::json!({
                                                    "role": "assistant",
                                                    "content": text
                                                }));
                                            }
                                        }
                                    }
                                    Some("tool_use") => {
                                        let tool_id = block.get("id")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("").to_string();
                                        let tool_name = block.get("name")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("unknown").to_string();
                                        let input = block.get("input")
                                            .cloned()
                                            .unwrap_or(serde_json::json!({}));
                                        let key_args = crate::acp::notifications::format_tool_title(&tool_name, &Some(input.clone()));
                                        pending_tools.insert(tool_id, (tool_name, key_args));
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    let start = if all_messages.len() > limit { all_messages.len() - limit } else { 0 };
    all_messages[start..].to_vec()
}

/// Delete a JSONL file by its session ID
pub fn delete_jsonl_by_session_id(agent_session_id: &str, storage_path: Option<&str>) -> bool {
    let storage_dir = match storage_path {
        Some(path) => {
            let expanded = shellexpand::tilde(path).to_string();
            std::path::PathBuf::from(expanded)
        }
        None => return false,
    };

    if let Some(path) = find_jsonl_path(agent_session_id, &storage_dir) {
        match std::fs::remove_file(&path) {
            Ok(()) => {
                tracing::info!("Deleted session file: {}", path.display());
                true
            }
            Err(e) => {
                tracing::warn!("Failed to delete session file {}: {}", path.display(), e);
                false
            }
        }
    } else {
        tracing::warn!("Session file not found for deletion: {}", agent_session_id);
        false
    }
}

/// Parse ISO 8601 timestamp to unix millis
/// e.g., "2026-03-26T04:37:44.598Z"
fn parse_iso_timestamp(ts: &str) -> Option<u64> {
    let ts = ts.trim_end_matches('Z').trim_end_matches('z');
    let parts: Vec<&str> = ts.split(|c: char| c == 'T' || c == ':' || c == '.' || c == '-').collect();
    if parts.len() < 6 {
        return None;
    }

    let year: i32 = parts[0].parse().ok()?;
    let month: u32 = parts[1].parse().ok()?;
    let day: u32 = parts[2].parse().ok()?;
    let hour: u32 = parts[3].parse().ok()?;
    let minute: u32 = parts[4].parse().ok()?;
    let second: u32 = parts[5].parse().unwrap_or(0);

    let days = days_from_epoch(year, month, day)?;
    let secs = (days as u64 * 86400) + (hour as u64 * 3600) + (minute as u64 * 60) + (second as u64);
    Some(secs * 1000)
}

/// Calculate days since 1970-01-01
fn days_from_epoch(year: i32, month: u32, day: u32) -> Option<u64> {
    if month < 1 || month > 12 || day < 1 || day > 31 {
        return None;
    }

    let mut days: i64 = 0;
    for y in 1970..year {
        days += if is_leap_year(y) { 366 } else { 365 };
    }

    let days_in_months = [0, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    for m in 1..month {
        days += days_in_months[m as usize] as i64;
        if m == 2 && is_leap_year(year) {
            days += 1;
        }
    }
    days += (day - 1) as i64;

    if days < 0 { return None; }
    Some(days as u64)
}

fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// Check if a message is a system/meta message that should be hidden from display
fn is_system_message(text: &str) -> bool {
    text.contains("<local-command-caveat>")
        || text.contains("<command-name>")
        || text.contains("<local-command-stdout>")
        || text.contains("<local-command-stderr>")
        || text.contains("This session is being continued from a previous conversation")
}
