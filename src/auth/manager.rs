//! Authorization manager for multi-client access
//!
//! Stores authorized clients in auth.json with per-client permissions.
//! Unlike binding (exclusive), authorization supports multiple clients.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};

/// Global authorization manager
static AUTH_MANAGER: Lazy<Arc<Mutex<AuthManager>>> = Lazy::new(|| {
    Arc::new(Mutex::new(AuthManager::new_internal()))
});

/// Get global auth manager
pub fn get_auth_manager() -> Arc<Mutex<AuthManager>> {
    AUTH_MANAGER.clone()
}

/// Authorized client info (stored in auth.json)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthorizedClient {
    pub id: String,
    pub name: String,
    pub token: String,
    pub created_at: i64,
    pub expires_at: Option<i64>,
    pub allowed_agents: Vec<String>,
    pub allowed_methods: Vec<String>,
    pub allow_system: bool,
}

/// Pending access request (consent flow, stored in pending_requests.json).
/// `token` is set on approval — the visitor's one-time fetch handoff slot:
/// `checkAccess` hands it over exactly once, then the request is removed.
/// camelCase = wire shape (§2.9 listRequests / pending_requests.json 同形).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AccessRequest {
    pub request_id: String,
    pub client_name: String,
    /// Agent the visitor asked to reach (from the 客服码 suffix), if any
    pub agent: Option<String>,
    pub created_at: i64,
    pub token: Option<String>,
}

/// listRequests wire view: token 永不出网关；`approved` = 票已铸、
/// 等访客 checkAccess 一次性取走（主人面板凭它区分"待审/已批待领取"）。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PendingRequestView {
    pub request_id: String,
    pub client_name: String,
    pub agent: Option<String>,
    pub created_at: i64,
    pub approved: bool,
}

/// Pending requests older than this are stale (visitor gave up polling)
const PENDING_TTL_SECS: i64 = 86400;
/// Upper bound on the pending queue (oldest dropped beyond this)
const MAX_PENDING_REQUESTS: usize = 50;

/// Authorization manager
pub struct AuthManager {
    data_dir: PathBuf,
    clients: HashMap<String, AuthorizedClient>,
    pending: Vec<AccessRequest>,
}

impl AuthManager {
    fn new_internal() -> Self {
        let data_dir = crate::config::data_dir();
        let clients = Self::load_clients(&data_dir);
        let mut pending = Self::load_pending(&data_dir);
        // Drop stale requests at boot (visitor polls stop long before a day)
        let now = chrono::Utc::now().timestamp();
        pending.retain(|r| now - r.created_at < PENDING_TTL_SECS);
        Self { data_dir, clients, pending }
    }

    fn clients_path(&self) -> PathBuf {
        self.data_dir.join("auth.json")
    }

    fn load_clients(data_dir: &Path) -> HashMap<String, AuthorizedClient> {
        let path = data_dir.join("auth.json");
        if !path.exists() {
            return HashMap::new();
        }
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("Failed to read auth.json: {}", e);
                return HashMap::new();
            }
        };
        let list: Vec<AuthorizedClient> = match serde_json::from_str(&content) {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!("Failed to parse auth.json: {}", e);
                return HashMap::new();
            }
        };
        list.into_iter().map(|c| (c.id.clone(), c)).collect()
    }

    fn save_clients(&self) -> anyhow::Result<()> {
        let path = self.clients_path();
        let list: Vec<&AuthorizedClient> = self.clients.values().collect();
        let content = serde_json::to_string_pretty(&list)?;
        crate::binding::write_secret_file(&path, &content)?;
        Ok(())
    }

    /// Add a new authorized client
    pub fn add_client(&mut self, client: AuthorizedClient) -> anyhow::Result<()> {
        self.clients.insert(client.id.clone(), client);
        self.save_clients()?;
        Ok(())
    }

    /// Remove an authorized client by ID
    pub fn remove_client(&mut self, id: &str) -> bool {
        let removed = self.clients.remove(id).is_some();
        if removed {
            if let Err(e) = self.save_clients() {
                tracing::warn!("Failed to save auth.json after removal: {}", e);
            }
        }
        removed
    }

    fn pending_path(&self) -> PathBuf {
        self.data_dir.join("pending_requests.json")
    }

    fn load_pending(data_dir: &Path) -> Vec<AccessRequest> {
        let path = data_dir.join("pending_requests.json");
        if !path.exists() {
            return Vec::new();
        }
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("Failed to read pending_requests.json: {}", e);
                return Vec::new();
            }
        };
        serde_json::from_str(&content).unwrap_or_else(|e| {
            tracing::warn!("Failed to parse pending_requests.json: {}", e);
            Vec::new()
        })
    }

    fn save_pending(&self) {
        let path = self.pending_path();
        if let Err(e) = crate::binding::write_secret_file(&path, &serde_json::to_string(&self.pending).unwrap_or_default()) {
            tracing::warn!("Failed to save pending_requests.json: {}", e);
        }
    }

    /// Mint a fresh AuthorizedClient (not yet stored — caller decides via
    /// add_client). Plain opaque token, no JWT: consent flow clients are
    /// issued by this gateway itself, auth.json is the source of truth.
    pub fn issue_client(
        &self,
        name: &str,
        allowed_agents: Vec<String>,
        expire_days: Option<i64>,
    ) -> AuthorizedClient {
        let now = chrono::Utc::now().timestamp();
        AuthorizedClient {
            id: format!("client-{}", &uuid::Uuid::new_v4().to_string()[..8]),
            name: name.to_string(),
            token: format!("ac-{}", uuid::Uuid::new_v4().to_string().replace('-', "")),
            created_at: now,
            expires_at: expire_days.map(|d| now + d * 86400),
            allowed_agents,
            // Empty = all non-system methods; system stays off for visitors
            allowed_methods: Vec::new(),
            allow_system: false,
        }
    }

    /// Queue a visitor's access request. Returns the stored request.
    pub fn add_request(&mut self, client_name: &str, agent: Option<&str>) -> AccessRequest {
        let req = AccessRequest {
            request_id: format!("req-{}", &uuid::Uuid::new_v4().to_string()[..8]),
            client_name: client_name.to_string(),
            agent: agent.map(|a| a.to_string()),
            created_at: chrono::Utc::now().timestamp(),
            token: None,
        };
        self.pending.push(req.clone());
        // Bounded queue: drop oldest beyond the cap
        while self.pending.len() > MAX_PENDING_REQUESTS {
            self.pending.remove(0);
        }
        self.save_pending();
        req
    }

    /// Pending requests (tokens stripped — handoff is checkAccess-only).
    pub fn list_requests(&self) -> Vec<AccessRequest> {
        self.pending
            .iter()
            .map(|r| AccessRequest { token: None, ..r.clone() })
            .collect()
    }

    /// Wire views for listRequests (adds the approved-without-token flag).
    pub fn list_request_views(&self) -> Vec<PendingRequestView> {
        self.pending
            .iter()
            .map(|r| PendingRequestView {
                request_id: r.request_id.clone(),
                client_name: r.client_name.clone(),
                agent: r.agent.clone(),
                created_at: r.created_at,
                approved: r.token.is_some(),
            })
            .collect()
    }

    /// Record the approved token on a pending request (visitor fetch slot).
    pub fn set_request_token(&mut self, request_id: &str, token: &str) {
        if let Some(r) = self.pending.iter_mut().find(|r| r.request_id == request_id) {
            r.token = Some(token.to_string());
            self.save_pending();
        }
    }

    /// One-time token handoff: returns the token and removes the request.
    /// 销单只在真有票时发生——未批准的请求必须留在队列里等主人
    /// （访客轮询先于批准是常态，首轮轮询就销单会让同意流永远走不通）。
    pub fn take_request_token(&mut self, request_id: &str) -> Option<String> {
        let token = self
            .pending
            .iter()
            .find(|r| r.request_id == request_id)?
            .token
            .clone()?;
        self.pending.retain(|r| r.request_id != request_id);
        self.save_pending();
        Some(token)
    }

    /// Drop a pending request (owner rejected / cleanup). Returns true if it existed.
    pub fn remove_request(&mut self, request_id: &str) -> bool {
        let before = self.pending.len();
        self.pending.retain(|r| r.request_id != request_id);
        let removed = before != self.pending.len();
        if removed {
            self.save_pending();
        }
        removed
    }

    /// Find client by token (constant-time comparison)
    pub fn find_by_token(&self, token: &str) -> Option<&AuthorizedClient> {
        self.clients.values().find(|c| {
            crate::binding::constant_time_eq(&c.token, token)
        })
    }

    /// List all authorized clients
    pub fn list_clients(&self) -> Vec<&AuthorizedClient> {
        self.clients.values().collect()
    }

    /// Clean expired clients and stale pending requests
    pub fn clean_expired(&mut self) -> usize {
        let now = chrono::Utc::now().timestamp();
        let before = self.clients.len();
        self.clients.retain(|_, c| {
            c.expires_at.map(|exp| now < exp).unwrap_or(true)
        });
        let removed = before - self.clients.len();
        if removed > 0 {
            if let Err(e) = self.save_clients() {
                tracing::warn!("Failed to save auth.json after cleanup: {}", e);
            }
        }
        let pending_before = self.pending.len();
        // 与启动时同一条 TTL 规则：批而未取的挂单（token 已铸）不因被批准
        // 而被清--访客还有剩余 TTL 窗口来取票，只按 created_at 过期
        self.pending.retain(|r| now - r.created_at < PENDING_TTL_SECS);
        if self.pending.len() != pending_before {
            self.save_pending();
        }
        removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mgr_in_tmp(tag: &str) -> AuthManager {
        let dir = std::env::temp_dir().join(format!("aginx-auth-test-{}-{}", tag, uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        AuthManager {
            data_dir: dir,
            clients: HashMap::new(),
            pending: Vec::new(),
        }
    }

    /// 访客轮询先于主人批准是常态：未批准的 take 必须返回 None 且**不销单**
    /// （否则首轮轮询就销单，主人 listRequests 永远是空，同意流死锁）。
    #[test]
    fn take_request_token_keeps_unapproved_pending() {
        let mut m = mgr_in_tmp("keep");
        let req = m.add_request("访客", Some("clone-creator"));
        // 轮询三次（未批准）
        for _ in 0..3 {
            assert_eq!(m.take_request_token(&req.request_id), None);
        }
        assert_eq!(m.list_requests().len(), 1, "未批准的请求必须留在队列");
        // 批准后再轮询：拿到票且销单
        m.set_request_token(&req.request_id, "ac-ticket");
        assert_eq!(m.take_request_token(&req.request_id), Some("ac-ticket".into()));
        assert!(m.list_requests().is_empty(), "取票后销单");
        assert_eq!(m.take_request_token(&req.request_id), None, "一次性：再取无票");
    }

    /// 不存在的请求 → None（不 panic 不残留）。
    #[test]
    fn take_request_token_unknown_id_is_none() {
        let mut m = mgr_in_tmp("unknown");
        m.add_request("访客", None);
        assert_eq!(m.take_request_token("req-nonexistent"), None);
        assert_eq!(m.list_requests().len(), 1);
    }

    /// clean_expired（挂在 listRequests/checkAccess 轮询上）：批而未取的
    /// 挂单不得被清（访客还有 TTL 窗口来取票）；过期挂单与过期凭证要被清。
    #[test]
    fn clean_expired_keeps_approved_awaiting_fetch() {
        let mut m = mgr_in_tmp("clean");
        let fresh = m.add_request("访客A", None);
        m.set_request_token(&fresh.request_id, "ac-ticket"); // 已批待领取
        let stale = m.add_request("访客B", None);
        // 手工把 stale 做旧（超过 PENDING_TTL_SECS）
        m.pending.last_mut().unwrap().created_at -= PENDING_TTL_SECS + 10;
        let removed = m.clean_expired();
        assert_eq!(removed, 0);
        assert_eq!(m.pending.len(), 1, "陈旧挂单清掉、批而未取的必须存活");
        assert_eq!(m.pending[0].request_id, fresh.request_id);
        assert_ne!(m.pending[0].request_id, stale.request_id);
        // 存活的挂单仍能正常取票
        assert_eq!(m.take_request_token(&fresh.request_id), Some("ac-ticket".into()));
    }

    /// 过期凭证（expires_at 已过）被 clean_expired 清掉。
    #[test]
    fn clean_expired_removes_expired_clients() {
        let mut m = mgr_in_tmp("clean-cli");
        let now = chrono::Utc::now().timestamp();
        m.add_client(AuthorizedClient {
            id: "c1".into(), name: "旧访客".into(), token: "t1".into(),
            created_at: now - 100, expires_at: Some(now - 1),
            allowed_agents: vec![], allowed_methods: vec![], allow_system: false,
        }).unwrap();
        assert_eq!(m.clean_expired(), 1);
        assert!(m.list_clients().is_empty());
    }

    /// 视图层 approved 标志跟随票据铸造翻转，token 永不外泄。
    #[test]
    fn list_request_views_approved_flag() {
        let mut m = mgr_in_tmp("views");
        let req = m.add_request("访客", None);
        assert!(!m.list_request_views()[0].approved, "未批准 → approved=false");
        m.set_request_token(&req.request_id, "ac-ticket");
        assert!(m.list_request_views()[0].approved, "已批待领取 → approved=true");
        // 序列化形状：camelCase 键 + 无 token 字段（金样本同形）
        let j = serde_json::to_value(m.list_request_views()).unwrap();
        assert!(j[0].get("requestId").is_some() && j[0].get("approved").is_some());
        assert!(j[0].get("token").is_none());
    }
}
