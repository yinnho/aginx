//! prompt 存根（丢件柜台）。
//!
//! 网关经手的每轮 prompt 先写存根再 spawn；轮善终销账，夭折（超时杀进程/
//! 客户端断连/spawn 失败）留底。2026-09-26 教训：120s 进程超时三杀 me 分身
//! 轮，40KB 报告正文零残留——agent 子进程死在回合结束前，它自己的持久化
//! 按设计不会发生。存根让网关侧永远有底可查、可重发。
//! 落盘失败只 warn 不挡投递：存根是保命底，不是投递前置条件。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 写存根成功的凭据：销账/留底都靠它。没写成（None）时调用方静默跳过。
#[derive(Debug, Clone)]
pub struct SpoolTicket {
    path: PathBuf,
}

impl SpoolTicket {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct SpoolEntry {
    agent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    /// spawn 用的解析后 cwd（重发时的定位线索）
    #[serde(skip_serializing_if = "Option::is_none")]
    cwd: Option<String>,
    /// RFC3339 UTC 秒
    received_at: String,
    /// in_flight → timeout / client_disconnected / spawn_failed / stdin_failed / agent_failed
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    prompt: String,
}

/// 丢件柜台（进程内无共享状态：文件名全局唯一，无需锁）
#[derive(Clone)]
pub struct PromptSpool {
    dir: PathBuf,
}

fn spool_dir() -> PathBuf {
    crate::config::data_dir().join("spool")
}

impl PromptSpool {
    /// 开柜：建目录（失败不炸，写时再试）
    pub fn open() -> Self {
        let s = Self { dir: spool_dir() };
        if let Err(e) = std::fs::create_dir_all(&s.dir) {
            tracing::warn!("prompt 存根目录建不了: {} ({})", s.dir.display(), e);
        }
        s
    }

    /// 收件即落盘（tmp+rename 原子写）。失败返 None + warn，绝不挡投递。
    pub fn write(
        &self,
        agent: &str,
        session_id: Option<&str>,
        cwd: Option<&str>,
        prompt: &str,
    ) -> Option<SpoolTicket> {
        // 同毫秒并发轮靠进程内序号分流
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or_default();

        let safe: String = agent
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let sub = self.dir.join(safe);
        if let Err(e) = std::fs::create_dir_all(&sub) {
            tracing::warn!("prompt 存根落盘失败（建目录）: {} ({})", sub.display(), e);
            return None;
        }

        let entry = SpoolEntry {
            agent: agent.to_string(),
            session_id: session_id.map(|s| s.to_string()),
            cwd: cwd.map(|s| s.to_string()),
            received_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            status: "in_flight".to_string(),
            error: None,
            prompt: prompt.to_string(),
        };
        let path = sub.join(format!("{millis:016}-{seq:06}.json"));
        let write = serde_json::to_vec(&entry)
            .map_err(|e| e.to_string())
            .and_then(|b| {
                let tmp = path.with_extension("json.tmp");
                std::fs::write(&tmp, b).map_err(|e| e.to_string())?;
                std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
            });
        match write {
            Ok(()) => {
                tracing::debug!("prompt 存根入柜: {}", path.display());
                Some(SpoolTicket { path })
            }
            Err(e) => {
                tracing::warn!("prompt 存根落盘失败: {} ({})", path.display(), e);
                None
            }
        }
    }

    /// 善终销账：prompt 已完整交给 agent（进程正常退出，含自报错——
    /// 内容已达对端，后续是对端自己的持久化责任）
    pub fn delivered(&self, ticket: Option<&SpoolTicket>) {
        if let Some(t) = ticket {
            if let Err(e) = std::fs::remove_file(t.path()) {
                // 销账失败不致命：留下的是多余的 in_flight 底，不是丢件
                tracing::debug!("prompt 存根销账失败: {} ({})", t.path().display(), e);
            }
        }
    }

    /// 夭折留底：改写 status/error，文件保留给人工重发
    pub fn lost(&self, ticket: Option<&SpoolTicket>, status: &str, error: &str) {
        let Some(t) = ticket else { return };
        let updated = std::fs::read(t.path())
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .map(|mut v| {
                v["status"] = serde_json::json!(status);
                v["error"] = serde_json::json!(error);
                v
            })
            .and_then(|v| serde_json::to_vec(&v).ok())
            .and_then(|b| {
                let tmp = t.path().with_extension("json.tmp");
                std::fs::write(&tmp, b).ok()?;
                std::fs::rename(&tmp, t.path()).ok()
            });
        match updated {
            Some(()) => tracing::warn!(
                "prompt 存根留底 [{}] — 内容未丢，可人工重发: {}",
                status,
                t.path().display()
            ),
            None => tracing::warn!("prompt 存根留底失败（原文仍在）: {}", t.path().display()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_spool() -> (PromptSpool, PathBuf) {
        let dir = tempfile::tempdir().unwrap().keep().join("spool");
        (PromptSpool { dir: dir.clone() }, dir)
    }

    #[test]
    fn write_then_delivered_removes_file() {
        let (s, dir) = test_spool();
        let ticket = s.write("me", Some("s_42"), None, "40KB 报告正文").unwrap();

        let files: Vec<_> = std::fs::read_dir(dir.join("me")).unwrap().collect();
        assert_eq!(files.len(), 1);

        s.delivered(Some(&ticket));
        assert!(std::fs::read_dir(dir.join("me")).unwrap().next().is_none());
        // None 凭据是 no-op，不炸
        s.delivered(None);
    }

    #[test]
    fn lost_keeps_status_and_prompt_intact() {
        let (s, dir) = test_spool();
        let ticket = s
            .write("me", None, Some("/home/x/ws"), "陈楠的报告：③ SKU 按钮仍在视口外")
            .unwrap();
        s.lost(
            Some(&ticket),
            "timeout",
            "Agent process timed out after 900s",
        );

        let path = ticket.path().to_path_buf();
        let v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["status"], "timeout");
        assert_eq!(
            v["error"], "Agent process timed out after 900s"
        );
        assert!(v["prompt"].as_str().unwrap().contains("SKU"));
        assert_eq!(v["cwd"], "/home/x/ws");
        assert!(path.exists(), "留底必须保文件");
        let _ = dir;
    }

    #[test]
    fn unwritable_dir_returns_none_not_panic() {
        // 柜台路径被一个同名文件占住 → create_dir_all 失败 → None，不挡投递
        let tmp = tempfile::tempdir().unwrap().keep();
        let blocker = tmp.join("spool");
        std::fs::write(&blocker, b"not a dir").unwrap();
        let s = PromptSpool { dir: blocker };
        assert!(s.write("me", None, None, "hello").is_none());
        s.lost(None, "timeout", "x"); // None no-op
    }

    #[test]
    fn rapid_writes_get_distinct_files() {
        let (s, dir) = test_spool();
        let a = s.write("me", None, None, "1").unwrap();
        let b = s.write("me", None, None, "2").unwrap();
        assert_ne!(a.path(), b.path());
        assert_eq!(std::fs::read_dir(dir.join("me")).unwrap().count(), 2);
    }
}
