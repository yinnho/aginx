//! App 绑定管理
//!
//! 管理 App 和 aginx 的绑定关系

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use serde::{Deserialize, Serialize};

/// 配对码有效期（5分钟）
const PAIR_CODE_TTL_SECS: i64 = 300;

/// 配对码长度
const PAIR_CODE_LENGTH: usize = 6;

/// 绑定管理器
pub struct BindingManager {
    /// 数据目录
    data_dir: PathBuf,
    /// 已绑定的设备
    devices: HashMap<String, DeviceInfo>,
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

impl BindingManager {
    /// 创建新的绑定管理器
    pub fn new() -> Self {
        let data_dir = Self::get_data_dir();

        // 确保目录存在
        let _ = fs::create_dir_all(&data_dir);

        let devices = Self::load_devices(&data_dir).unwrap_or_default();

        Self {
            data_dir,
            devices,
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
    fn devices_path(&self) -> PathBuf {
        self.data_dir.join("bindings.json")
    }

    /// 加载设备列表
    fn load_devices(data_dir: &PathBuf) -> anyhow::Result<HashMap<String, DeviceInfo>> {
        let path = data_dir.join("bindings.json");
        let content = fs::read_to_string(path)?;
        let devices: Vec<DeviceInfo> = serde_json::from_str(&content)?;
        Ok(devices.into_iter().map(|d| (d.id.clone(), d)).collect())
    }

    /// 保存设备列表
    fn save_devices(&self) -> anyhow::Result<()> {
        let devices: Vec<_> = self.devices.values().cloned().collect();
        let content = serde_json::to_string_pretty(&devices)?;
        fs::write(self.devices_path(), content)?;
        Ok(())
    }

    /// 保存配对码
    fn save_pair_code(&self, code: &str) -> anyhow::Result<()> {
        let data = PairCodeData {
            created_at: chrono::Utc::now().timestamp(),
        };
        let content = serde_json::to_string(&data)?;
        fs::write(self.pair_code_path(), content)?;
        Ok(())
    }

    /// 加载配对码
    fn load_pair_code(&self, code: &str) -> Option<PairCodeData> {
        let path = self.pair_code_path();
        let content = fs::read_to_string(path).ok()?;
        let data: PairCodeData = serde_json::from_str(&content).ok()?;

        // 检查是否过期
        let now = chrono::Utc::now().timestamp();
        if now - data.created_at > PAIR_CODE_TTL_SECS {
            return None;
        }

        Some(data)
    }

    /// 删除配对码
    fn delete_pair_code(&self) {
        let _ = fs::remove_file(self.pair_code_path());
    }

    /// 生成配对码
    pub fn generate_pair_code(&mut self) -> PairResult {
        // 生成随机配对码
        let code = self.generate_random_code();

        // 保存到文件
        let _ = self.save_pair_code(&code);

        PairResult {
            code,
            expires_in: PAIR_CODE_TTL_SECS as u64,
        }
    }

    /// 验证配对码
    pub fn verify_pair_code(&mut self, code: &str, device_name: &str) -> Option<DeviceInfo> {
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
            self.devices.insert(device.id.clone(), device.clone());
            let _ = self.save_devices();

            return Some(device);
        }

        None
    }

    /// 获取设备列表
    pub fn list_devices(&self) -> Vec<DeviceInfo> {
        self.devices.values().cloned().collect()
    }

    /// 解绑设备
    pub fn unbind_device(&mut self, device_id: &str) -> bool {
        if self.devices.remove(device_id).is_some() {
            let _ = self.save_devices();
            true
        } else {
            false
        }
    }

    /// 验证 Token
    pub fn verify_token(&self, token: &str) -> Option<&DeviceInfo> {
        self.devices.values().find(|d| d.token == token)
    }

    /// 更新设备活跃时间
    pub fn update_last_active(&mut self, device_id: &str) {
        if let Some(device) = self.devices.get_mut(device_id) {
            device.last_active = chrono::Utc::now().timestamp();
            let _ = self.save_devices();
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

// Re-export for use in main.rs
pub use self::BindingManager as BindingManagerAlias;
