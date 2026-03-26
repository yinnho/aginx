//! App 绑定管理
//!
//! 管理 App 和 aginx 的绑定关系
//! 一个 aginx 只能被一个 App 绑定（独占模式）

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use serde::{Deserialize, Serialize};
use once_cell::sync::Lazy;

/// 配对码有效期（5分钟）
const PAIR_CODE_TTL_SECS: i64 = 300;

/// 配对码长度
const PAIR_CODE_LENGTH: usize = 6;

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

    /// 兼容旧接口 - 创建新的绑定管理器
    /// 注意：推荐使用 get_binding_manager() 获取单例
    pub fn new() -> Self {
        // 从单例复制状态
        let manager = get_binding_manager();
        let singleton = manager.lock().unwrap();
        Self {
            data_dir: singleton.data_dir.clone(),
            bound_device: singleton.bound_device.clone(),
        }
    }

    /// 获取数据目录
    fn get_data_dir() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home).join(".aginx")
    }

    /// 配对码文件路径
    fn pair_code_path(&self) -> PathBuf {
        self.data_dir.join("pair_code.json")
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
            fs::write(self.device_path(), content)?;
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
        fs::write(self.pair_code_path(), content)?;
        Ok(())
    }

    /// 加载并验证配对码
    fn load_pair_code(&self, code: &str) -> Option<PairCodeData> {
        let path = self.pair_code_path();
        let content = fs::read_to_string(path).ok()?;
        let data: PairCodeData = serde_json::from_str(&content).ok()?;

        // 验证配对码是否匹配
        if data.code != code {
            tracing::warn!("配对码不匹配: 输入={}, 存储={}", code, data.code);
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

    /// 验证配对码并绑定设备（独占模式）
    /// 如果已经有绑定的设备，返回 AlreadyBound
    pub fn bind_device(&mut self, code: &str, device_name: &str) -> BindResult {
        // 检查是否已被绑定
        if let Some(ref device) = self.bound_device {
            return BindResult::AlreadyBound {
                device_name: device.name.clone(),
            };
        }

        // 加载并验证配对码
        if self.load_pair_code(code).is_some() {
            // 删除配对码（一次性使用）
            self.delete_pair_code();

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

        BindResult::InvalidCode
    }

    /// 兼容旧接口
    pub fn verify_pair_code(&mut self, code: &str, device_name: &str) -> Option<DeviceInfo> {
        match self.bind_device(code, device_name) {
            BindResult::Success(device) => Some(device),
            _ => None,
        }
    }

    /// 获取已绑定设备
    pub fn get_bound_device(&self) -> Option<&DeviceInfo> {
        self.bound_device.as_ref()
    }

    /// 获取设备列表（兼容旧接口，返回单个设备的数组）
    pub fn list_devices(&self) -> Vec<DeviceInfo> {
        self.bound_device.iter().cloned().collect()
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

    /// 验证 Token
    pub fn verify_token(&self, token: &str) -> Option<&DeviceInfo> {
        self.bound_device.as_ref().filter(|d| d.token == token)
    }

    /// 更新设备活跃时间
    pub fn update_last_active(&mut self) {
        if let Some(ref mut device) = self.bound_device {
            device.last_active = chrono::Utc::now().timestamp();
            if let Err(e) = self.save_device() {
                tracing::warn!("更新活跃时间失败: {}", e);
            }
        }
    }

    /// 生成随机配对码（6位数字）
    fn generate_random_code(&self) -> String {
        use rand::Rng;
        let mut rng = rand::thread_rng();
        (0..PAIR_CODE_LENGTH)
            .map(|_| rng.gen_range(0..10).to_string())
            .collect()
    }
}

impl Default for BindingManager {
    fn default() -> Self {
        Self::new()
    }
}
