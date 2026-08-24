//! 会话台账（ACP.md §2.4.1 的事实源）。
//!
//! 网关经手的每轮 prompt，用翻译器收割到的 agent 真会话 id 记账：
//! {注册名, sessionId, title=首句, lastTs, turns}，按注册名查询、时间倒序。
//! 纯通用记账——零方言知识；raw 方言收割不到 id，台账自然为空。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

/// 台账条目（wire 形状 camelCase，sessions/list 直出）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(non_snake_case)]
pub struct SessionSummary {
    /// agent 真会话 id（翻译器收割，如 claude 的 session_id）
    pub sessionId: String,
    /// 首条 prompt 截断
    pub title: String,
    /// 最后一次经手时间（RFC3339 UTC 秒）
    pub lastTs: String,
    /// 网关经手的轮数
    pub turns: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct LedgerFile {
    /// key = "<agent>/<sessionId>"
    entries: HashMap<String, StoredEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredEntry {
    agent: String,
    #[serde(flatten)]
    summary: SessionSummary,
}

/// 进程内共享 + 落盘的台账
#[derive(Clone)]
pub struct SessionLedger {
    inner: Arc<RwLock<LedgerFile>>,
    path: PathBuf,
}

fn ledger_path() -> PathBuf {
    crate::config::data_dir().join("sessions.json")
}

impl SessionLedger {
    /// 加载（文件缺失/损坏 → 空台账 + warn，不炸启动）
    pub fn load() -> Self {
        let path = ledger_path();
        let file = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<LedgerFile>(&b).ok());
        match file {
            Some(f) => Self {
                inner: Arc::new(RwLock::new(f)),
                path,
            },
            None => {
                if path.exists() {
                    tracing::warn!("会话台账损坏，重置: {}", path.display());
                }
                Self {
                    inner: Arc::new(LedgerFile::default().into()),
                    path,
                }
            }
        }
    }

    /// 记一轮：已有条目 → turns+1/lastTs 更新；新会话 → 建条目（title=首句截断）。
    /// message 用本轮 prompt 原文。
    pub fn record_turn(&self, agent: &str, session_id: &str, message: &str) {
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let key = format!("{}/{}", agent, session_id);
        let mut f = self.inner.write().unwrap();
        match f.entries.get_mut(&key) {
            Some(e) => {
                e.summary.turns += 1;
                e.summary.lastTs = now;
            }
            None => {
                f.entries.insert(
                    key,
                    StoredEntry {
                        agent: agent.to_string(),
                        summary: SessionSummary {
                            sessionId: session_id.to_string(),
                            title: truncate_chars(message, 60),
                            lastTs: now,
                            turns: 1,
                        },
                    },
                );
            }
        }
        self.persist(&f);
    }

    /// 按注册名列会话，lastTs 倒序
    pub fn list(&self, agent: &str) -> Vec<SessionSummary> {
        let f = self.inner.read().unwrap();
        let mut v: Vec<SessionSummary> = f
            .entries
            .values()
            .filter(|e| e.agent == agent)
            .map(|e| e.summary.clone())
            .collect();
        v.sort_by(|a, b| b.lastTs.cmp(&a.lastTs));
        v
    }

    /// 原子落盘（tmp+rename）；失败只 warn（台账丢一轮不致命）
    fn persist(&self, f: &LedgerFile) {
        let tmp = self.path.with_extension("json.tmp");
        let write = serde_json::to_string_pretty(f)
            .map_err(|e| e.to_string())
            .and_then(|s| std::fs::write(&tmp, s).map_err(|e| e.to_string()))
            .and_then(|_| std::fs::rename(&tmp, &self.path).map_err(|e| e.to_string()));
        if let Err(e) = write {
            tracing::warn!("会话台账落盘失败: {}", e);
        }
    }
}

/// chars 边界安全截断（&s[..N] 中文必炸的老坑）
fn truncate_chars(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let t: String = s.chars().take(n).collect();
        format!("{t}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_ledger() -> (SessionLedger, PathBuf) {
        // into_path()：TempDir 转永久路径（tempdir 句柄 drop 会即删目录，
        // persist 就写进坟场了）
        let dir = tempfile::tempdir().unwrap().keep();
        let path = dir.join("sessions.json");
        (
            SessionLedger {
                inner: Arc::new(LedgerFile::default().into()),
                path: path.clone(),
            },
            path,
        )
    }

    #[test]
    fn record_creates_updates_and_lists_desc() {
        let (l, path) = test_ledger();
        l.record_turn("quanyi", "s1", "第一条消息");
        l.record_turn("quanyi", "s1", "第二条消息"); // 同会话续轮
        l.record_turn("quanyi", "s2", "另一个会话");
        l.record_turn("claude", "s3", "别的注册项");

        let q = l.list("quanyi");
        assert_eq!(q.len(), 2, "按注册名隔离");
        let s1 = q.iter().find(|s| s.sessionId == "s1").unwrap();
        assert_eq!(s1.turns, 2, "续轮 turns+1");
        assert_eq!(s1.title, "第一条消息", "title 锁首句");
        assert_eq!(l.list("claude").len(), 1);
        assert_eq!(l.list("nobody").len(), 0);

        // 落盘 round-trip
        let reloaded: LedgerFile =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(reloaded.entries.len(), 3);
    }

    #[test]
    fn long_title_truncated_char_safe() {
        let (l, path) = test_ledger();
        l.record_turn("a", "s", &"很长的一句话".repeat(30));
        let t = l.list("a")[0].title.clone();
        assert!(t.chars().count() <= 61 && t.ends_with('…'));
        assert!(path.exists());
    }
}
