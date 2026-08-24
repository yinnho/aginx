//! CLI stdout 方言翻译器（接入包 `output` 声明驱动，ACP.md §2.8）。
//!
//! 方言知识全部住本模块的翻译器注册表里——网关核心只认 `OutputTranslator`
//! 接口，零 CLI 字样。新增 CLI 方言 = 加一个翻译器 + 接入包声明一行。

/// 接入包声明的输出方言
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputFormat {
    /// 裸文本行：直通（缺省，向后兼容）
    #[default]
    Raw,
    /// claude `--output-format stream-json --verbose`
    ClaudeStreamJson,
}

impl OutputFormat {
    /// 从 aginx.toml 的 output 声明解析；未知值回落 Raw 并 warn（不炸启动）。
    pub fn parse(decl: Option<&str>) -> Self {
        match decl {
            None | Some("raw") => Self::Raw,
            Some("claude-stream-json") => Self::ClaudeStreamJson,
            Some(other) => {
                tracing::warn!(output = %other, "未知 output 方言，回落 raw 直通");
                Self::Raw
            }
        }
    }
}

/// 一行 stdout 的翻译结果
#[derive(Debug, Default)]
pub struct TranslatedLine {
    /// 翻译后的纯文本 chunk（0..n；拼起来即本轮文本）
    pub chunks: Vec<String>,
    /// 收割到的 agent 真会话 id（claude result 行）
    pub session_id: Option<String>,
    /// 翻译器附带的成本/时长/轮数（§2.5 可选字段）
    pub cost_usd: Option<f64>,
    pub duration_ms: Option<u64>,
    pub num_turns: Option<u64>,
    /// agent 自报失败（claude is_error:true）
    pub is_error: bool,
    pub error_text: Option<String>,
}

/// 翻译一行 CLI stdout。非法 JSON 在 claude 方言下按裸文本直通（容错）。
pub fn translate_line(format: OutputFormat, line: &str) -> TranslatedLine {
    match format {
        OutputFormat::Raw => {
            let mut t = TranslatedLine::default();
            if !line.is_empty() {
                t.chunks.push(line.to_string());
            }
            t
        }
        OutputFormat::ClaudeStreamJson => translate_claude_line(line),
    }
}

fn sanitize_session_id(raw: &str) -> Option<String> {
    let ok = !raw.is_empty()
        && raw
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    ok.then(|| raw.to_string())
}

fn translate_claude_line(line: &str) -> TranslatedLine {
    let mut t = TranslatedLine::default();
    let v: serde_json::Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => {
            // 非法 JSON：按裸文本直通（claude stream-json 下偶发杂音不炸流）
            if !line.is_empty() {
                t.chunks.push(line.to_string());
            }
            return t;
        }
    };
    match v.get("type").and_then(|s| s.as_str()) {
        // assistant 行：message.content[] 的 text 块拼一段发出（§2.6 纯文本）。
        // thinking/tool_use 块跳过——外部协议只有文本流。
        Some("assistant") => {
            let mut text = String::new();
            if let Some(blocks) = v.pointer("/message/content").and_then(|c| c.as_array()) {
                for b in blocks {
                    if b.get("type").and_then(|s| s.as_str()) == Some("text") {
                        if let Some(s) = b.get("text").and_then(|s| s.as_str()) {
                            text.push_str(s);
                        }
                    }
                }
            }
            if !text.is_empty() {
                t.chunks.push(text);
            }
        }
        // result 行：收割真 session_id + 成本元数据（§2.5 立法语义）
        Some("result") => {
            if let Some(sid) = v.get("session_id").and_then(|s| s.as_str()) {
                t.session_id = sanitize_session_id(sid);
            }
            t.cost_usd = v.get("total_cost_usd").and_then(|c| c.as_f64());
            t.duration_ms = v.get("duration_ms").and_then(|c| c.as_u64());
            t.num_turns = v.get("num_turns").and_then(|c| c.as_u64());
            if v.get("is_error").and_then(|e| e.as_bool()).unwrap_or(false) {
                t.is_error = true;
                t.error_text = v
                    .get("result")
                    .and_then(|r| r.as_str())
                    .map(|s| s.to_string())
                    .or_else(|| Some("claude reported is_error".into()));
            }
        }
        // system/stream_event 等其余行：静默忽略
        _ => {}
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_assistant_text_blocks_become_chunks() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"…"},{"type":"text","text":"你好"},{"type":"tool_use","name":"Bash"}]}}"#;
        let t = translate_line(OutputFormat::ClaudeStreamJson, line);
        assert_eq!(t.chunks, vec!["你好".to_string()]);
        assert!(t.session_id.is_none());
    }

    #[test]
    fn claude_result_line_harvests_everything() {
        let line = r#"{"type":"result","subtype":"success","session_id":"b8f713a4-ea3b-4d6c-920b-87dc2a0403f0","total_cost_usd":0.015,"duration_ms":8400,"num_turns":1,"is_error":false,"result":"done"}"#;
        let t = translate_line(OutputFormat::ClaudeStreamJson, line);
        assert_eq!(
            t.session_id.as_deref(),
            Some("b8f713a4-ea3b-4d6c-920b-87dc2a0403f0")
        );
        assert_eq!(t.cost_usd, Some(0.015));
        assert_eq!(t.duration_ms, Some(8400));
        assert_eq!(t.num_turns, Some(1));
        assert!(!t.is_error);
        assert!(t.chunks.is_empty(), "result 行不产文本");
    }

    #[test]
    fn claude_error_result_flags() {
        let line = r#"{"type":"result","subtype":"error_max_turns","session_id":"abc","is_error":true,"result":"达到轮数上限"}"#;
        let t = translate_line(OutputFormat::ClaudeStreamJson, line);
        assert!(t.is_error);
        assert_eq!(t.error_text.as_deref(), Some("达到轮数上限"));
    }

    #[test]
    fn claude_system_and_garbage_ignored_or_passed() {
        let t = translate_line(OutputFormat::ClaudeStreamJson, r#"{"type":"system","subtype":"init","session_id":"x"}"#);
        assert!(t.chunks.is_empty() && t.session_id.is_none());
        let t = translate_line(OutputFormat::ClaudeStreamJson, "not json at all");
        assert_eq!(t.chunks, vec!["not json at all".to_string()]);
    }

    #[test]
    fn session_id_injection_rejected() {
        let line = r#"{"type":"result","session_id":"a;rm -rf /"}"#;
        let t = translate_line(OutputFormat::ClaudeStreamJson, line);
        assert!(t.session_id.is_none());
    }

    #[test]
    fn raw_passthrough_and_empty() {
        let t = translate_line(OutputFormat::Raw, "hello");
        assert_eq!(t.chunks, vec!["hello".to_string()]);
        let t = translate_line(OutputFormat::Raw, "");
        assert!(t.chunks.is_empty());
    }

    #[test]
    fn format_parse_and_unknown_falls_back() {
        assert_eq!(OutputFormat::parse(None), OutputFormat::Raw);
        assert_eq!(OutputFormat::parse(Some("raw")), OutputFormat::Raw);
        assert_eq!(
            OutputFormat::parse(Some("claude-stream-json")),
            OutputFormat::ClaudeStreamJson
        );
        assert_eq!(OutputFormat::parse(Some("gemini-vibe")), OutputFormat::Raw);
    }
}
