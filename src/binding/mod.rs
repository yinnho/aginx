//! App 绑定管理
//!
//! 管理 App 和 aginx 的绑定关系
//! 一个 aginx 只能被一个 App 绑定（独占模式）

use std::fs;
use std::path::{PathBuf, Path};
use std::sync::{Arc, Mutex};
use serde::{Deserialize, Serialize};
use once_cell::sync::Lazy;

/// 配对码有效期（5分钟）
const PAIR_CODE_TTL_SECS: i64 = 300;

/// 配对码长度
const PAIR_CODE_LENGTH: usize = 6;

/// Write a file with restrictive permissions (0600 on Unix).
fn write_secret_file(path: &Path, content: &str) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        use std::io::Write;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?
            .write_all(content.as_bytes())?;
    }
    #[cfg(not(unix))]
    {
        fs::write(path, content)?;
    }
    Ok(())
}

/// 最大失败尝试次数
const MAX_FAILED_ATTEMPTS: u32 = 5;

/// Constant-time string comparison to prevent timing attacks.
/// Returns true if both strings are equal.
fn constant_time_eq(a: &str, b: &str) -> bool {
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();
    let mut result = if a_bytes.len() == b_bytes.len() { 0u8 } else { 0xFF };

    // XOR all byte pairs (shorter iter ends first, rest of longer is ignored)
    for (x, y) in a_bytes.iter().zip(b_bytes.iter()) {
        result |= x ^ y;
    }
    result == 0
}

/// 失败尝试锁定时间（15分钟）
const FAILED_ATTEMPTS_LOCKOUT_SECS: i64 = 900;

/// 全局单例 BindingManager
static BINDING_MANAGER: Lazy<Arc<Mutex<BindingManager>>> = Lazy::new(|| {
    Arc::new(Mutex::new(BindingManager::new_internal()))
});

/// 获取全局 BindingManager 单例
pub fn get_binding_manager() -> Arc<Mutex<BindingManager>> {
    BINDING_MANAGER.clone()
}

/// 绑定管理器
pub struct BindingManager {
    /// 数据目录
    data_dir: PathBuf,
    /// 已绑定的设备（独占模式，只能有一个）
    bound_device: Option<DeviceInfo>,
}

/// 设备信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfo {
    /// 设备 ID
    pub id: String,
    /// 设备名称
    pub name: String,
    /// 绑定时间
    pub bound_at: i64,
    /// 最后活跃时间
    pub last_active: i64,
    /// Token
    pub token: String,
}

/// 配对请求（存储用）
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PairCodeData {
    /// 配对码
    code: String,
    /// 创建时间（Unix 时间戳）
    created_at: i64,
}

/// 失败尝试记录
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FailedAttemptsData {
    /// 失败次数
    count: u32,
    /// 最近一次失败时间
    last_failure: i64,
}

/// 配对结果
#[derive(Debug)]
pub struct PairResult {
    /// 配对码
    pub code: String,
    /// 过期时间（秒）
    pub expires_in: u64,
}

/// 绑定结果
#[derive(Debug)]
pub enum BindResult {
    /// 绑定成功
    Success(DeviceInfo),
    /// 已被其他设备绑定
    AlreadyBound { device_name: String },
    /// 配对码无效或已过期
    InvalidCode,
}

#[allow(dead_code)]
impl BindingManager {
    /// 创建新的绑定管理器（内部方法，使用 get_binding_manager 获取单例）
    fn new_internal() -> Self {
        let data_dir = Self::get_data_dir();

        // 确保目录存在
        if let Err(e) = fs::create_dir_all(&data_dir) {
            tracing::warn!("创建数据目录失败: {}", e);
        }

        let bound_device = Self::load_device(&data_dir).unwrap_or(None);

        Self {
            data_dir,
            bound_device,
        }
    }

    /// 获取数据目录
    fn get_data_dir() -> PathBuf {
        crate::config::data_dir()
    }

    /// 配对码文件路径
    fn pair_code_path(&self) -> PathBuf {
        self.data_dir.join("pair_code.json")
    }

    /// 失败尝试记录文件路径
    fn failed_attempts_path(&self) -> PathBuf {
        self.data_dir.join("failed_attempts.json")
    }

    /// 设备文件路径
    fn device_path(&self) -> PathBuf {
        self.data_dir.join("binding.json")
    }

    /// 加载已绑定设备
    fn load_device(data_dir: &PathBuf) -> anyhow::Result<Option<DeviceInfo>> {
        let path = data_dir.join("binding.json");
        if !path.exists() {
            return Ok(None);
        }
        let content = fs::read_to_string(path)?;
        let device: DeviceInfo = serde_json::from_str(&content)?;
        Ok(Some(device))
    }

    /// 保存已绑定设备
    fn save_device(&self) -> anyhow::Result<()> {
        if let Some(ref device) = self.bound_device {
            let content = serde_json::to_string_pretty(device)?;
            write_secret_file(&self.device_path(), &content)?;
        } else {
            // 如果没有绑定设备，删除文件
            if let Err(e) = fs::remove_file(self.device_path()) {
                // 文件不存在是正常情况，不需要警告
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!("删除绑定文件失败: {}", e);
                }
            }
        }
        Ok(())
    }

    /// 保存配对码
    fn save_pair_code(&self, code: &str) -> anyhow::Result<()> {
        let data = PairCodeData {
            code: code.to_string(),
            created_at: chrono::Utc::now().timestamp(),
        };
        let content = serde_json::to_string(&data)?;
        write_secret_file(&self.pair_code_path(), &content)?;
        Ok(())
    }

    /// 加载并验证配对码
    fn load_pair_code(&self, code: &str) -> Option<PairCodeData> {
        let path = self.pair_code_path();
        let content = fs::read_to_string(path).ok()?;
        let data: PairCodeData = serde_json::from_str(&content).ok()?;

        // 验证配对码是否匹配 (constant-time)
        if !constant_time_eq(&data.code, code) {
            return None;
        }

        // 检查是否过期
        let now = chrono::Utc::now().timestamp();
        if now - data.created_at > PAIR_CODE_TTL_SECS {
            tracing::warn!("配对码已过期");
            return None;
        }

        Some(data)
    }

    /// 删除配对码
    fn delete_pair_code(&self) {
        if let Err(e) = fs::remove_file(self.pair_code_path()) {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!("删除配对码文件失败: {}", e);
            }
        }
    }

    /// 加载失败尝试记录
    fn load_failed_attempts(&self) -> Option<FailedAttemptsData> {
        let path = self.failed_attempts_path();
        let content = fs::read_to_string(path).ok()?;
        serde_json::from_str(&content).ok()?
    }

    /// 保存失败尝试记录
    fn save_failed_attempts(&self, data: &FailedAttemptsData) -> anyhow::Result<()> {
        let content = serde_json::to_string(data)?;
        write_secret_file(&self.failed_attempts_path(), &content)?;
        Ok(())
    }

    /// 清除失败尝试记录
    fn clear_failed_attempts(&self) {
        if let Err(e) = fs::remove_file(self.failed_attempts_path()) {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!("清除失败尝试记录失败: {}", e);
            }
        }
    }

    /// 检查是否被临时锁定
    fn is_locked_out(&self) -> bool {
        if let Some(data) = self.load_failed_attempts() {
            let now = chrono::Utc::now().timestamp();
            // 如果在锁定时间内
            if data.count >= MAX_FAILED_ATTEMPTS {
                let lockout_end = data.last_failure + FAILED_ATTEMPTS_LOCKOUT_SECS;
                if now < lockout_end {
                    tracing::warn!("配对功能已被临时锁定，剩余 {} 秒", lockout_end - now);
                    return true;
                }
                // 锁定已过期，清除记录
                self.clear_failed_attempts();
            }
        }
        false
    }

    /// 记录一次失败的配对尝试
    fn record_failed_attempt(&self) {
        let now = chrono::Utc::now().timestamp();
        let mut data = self.load_failed_attempts().unwrap_or(FailedAttemptsData {
            count: 0,
            last_failure: now,
        });

        data.count += 1;
        data.last_failure = now;

        if let Err(e) = self.save_failed_attempts(&data) {
            tracing::warn!("保存失败尝试记录失败: {}", e);
        }

        if data.count >= MAX_FAILED_ATTEMPTS {
            tracing::warn!("失败尝试次数过多，配对功能已锁定 {} 秒", FAILED_ATTEMPTS_LOCKOUT_SECS);
        }
    }

    /// 生成配对码
    pub fn generate_pair_code(&mut self) -> PairResult {
        // 生成随机配对码
        let code = self.generate_random_code();

        // 保存到文件
        if let Err(e) = self.save_pair_code(&code) {
            tracing::error!("保存配对码失败: {}", e);
        }

        PairResult {
            code,
            expires_in: PAIR_CODE_TTL_SECS as u64,
        }
    }

    /// 检查 binding.json 文件是否已被外部删除，同步内存状态
    fn sync_with_file(&mut self) {
        if self.bound_device.is_some() && !self.device_path().exists() {
            tracing::info!("检测到 binding.json 被外部删除，同步清除内存绑定状态");
            self.bound_device = None;
        }
    }

    /// 验证配对码并绑定设备（独占模式）
    /// 如果已经有绑定的设备，返回 AlreadyBound
    pub fn bind_device(&mut self, code: &str, device_name: &str) -> BindResult {
        // 同步文件状态（可能被 aginx unbind 删除）
        self.sync_with_file();

        // 检查是否已被绑定
        if let Some(ref device) = self.bound_device {
            return BindResult::AlreadyBound {
                device_name: device.name.clone(),
            };
        }

        // 检查是否被临时锁定
        if self.is_locked_out() {
            return BindResult::InvalidCode;
        }

        // 加载并验证配对码
        if self.load_pair_code(code).is_some() {
            // 删除配对码（一次性使用）
            self.delete_pair_code();
            // 清除失败尝试记录
            self.clear_failed_attempts();

            // 生成设备 ID 和 Token
            let device_id = format!("device-{}", uuid::Uuid::new_v4().to_string().split('-').next().unwrap_or("unknown"));
            let token = format!("token-{}", uuid::Uuid::new_v4().to_string().replace("-", ""));

            let device = DeviceInfo {
                id: device_id,
                name: device_name.to_string(),
                bound_at: chrono::Utc::now().timestamp(),
                last_active: chrono::Utc::now().timestamp(),
                token,
            };

            // 保存
            self.bound_device = Some(device.clone());
            if let Err(e) = self.save_device() {
                tracing::error!("保存绑定信息失败: {}", e);
            }

            tracing::info!("设备绑定成功: {} ({})", device.name, device.id);
            return BindResult::Success(device);
        }

        // 记录失败尝试
        self.record_failed_attempt();
        BindResult::InvalidCode
    }

    /// 获取已绑定设备
    pub fn get_bound_device(&self) -> Option<&DeviceInfo> {
        self.bound_device.as_ref()
    }

    /// 解绑设备
    pub fn unbind_device(&mut self, _device_id: &str) -> bool {
        if self.bound_device.is_some() {
            self.bound_device = None;
            if let Err(e) = self.save_device() {
                tracing::error!("保存解绑状态失败: {}", e);
            }
            tracing::info!("设备已解绑");
            true
        } else {
            false
        }
    }

    /// 解绑所有设备（直接清空）
    pub fn unbind_all(&mut self) {
        self.bound_device = None;
        if let Err(e) = self.save_device() {
            tracing::error!("保存解绑状态失败: {}", e);
        }
        tracing::info!("所有设备已解绑");
    }

    /// 验证 Token (constant-time comparison)
    /// 同时检查 binding.json 文件是否还存在（被外部 aginx unbind 删除时自动感知）
    pub fn verify_token(&mut self, token: &str) -> Option<DeviceInfo> {
        self.sync_with_file();
        self.bound_device.as_ref()
            .filter(|d| constant_time_eq(&d.token, token))
            .cloned()
    }

    #[allow(dead_code)]
    /// 更新设备活跃时间
    pub fn update_last_active(&mut self) {
        if let Some(ref mut device) = self.bound_device {
            device.last_active = chrono::Utc::now().timestamp();
            if let Err(e) = self.save_device() {
                tracing::warn!("更新活跃时间失败: {}", e);
            }
        }
    }

    /// 生成随机配对码（6位字母数字，区分大小写）
    fn generate_random_code(&self) -> String {
        use rand::Rng;
        // Alphanumeric excluding visually ambiguous chars (0/O, 1/l/I)
        const CHARSET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghjkmnpqrstuvwxyz23456789";
        let mut rng = rand::thread_rng();
        (0..PAIR_CODE_LENGTH)
            .map(|_| CHARSET[rng.gen_range(0..CHARSET.len())] as char)
            .collect()
    }
}