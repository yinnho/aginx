//! 设备硬件指纹生成模块
//!
//! 生成设备唯一的硬件指纹，用于：
//! 1. 设备身份认证
//! 2. 防止 aginx_id 被假冒
//! 3. 重置后恢复原 aginx_id

use sha2::{Digest, Sha256};

/// 设备硬件指纹
#[derive(Debug, Clone)]
pub struct HardwareFingerprint {
    /// 原始数据
    pub mac_address: Option<String>,
    pub disk_serial: Option<String>,
    pub motherboard_uuid: Option<String>,
    /// 生成的指纹 hash
    pub fingerprint: String,
}

impl HardwareFingerprint {
    /// 生成当前设备的硬件指纹
    pub fn generate() -> Self {
        let mac_address = get_mac_address();
        let disk_serial = get_disk_serial();
        let motherboard_uuid = get_motherboard_uuid();

        // 组合所有可用的硬件信息
        let mut components = Vec::new();
        if let Some(ref mac) = mac_address {
            components.push(mac.clone());
        }
        if let Some(ref disk) = disk_serial {
            components.push(disk.clone());
        }
        if let Some(ref uuid) = motherboard_uuid {
            components.push(uuid.clone());
        }

        // 生成 SHA256 hash
        let combined = components.join("|");
        let fingerprint = if combined.is_empty() {
            // 如果没有任何硬件信息，使用随机值（不推荐，但作为 fallback）
            tracing::warn!("无法获取任何硬件信息，使用随机指纹");
            format!("random-{}", uuid::Uuid::new_v4())
        } else {
            let mut hasher = Sha256::new();
            hasher.update(combined.as_bytes());
            let result = hasher.finalize();
            hex::encode(result)
        };

        Self {
            mac_address,
            disk_serial,
            motherboard_uuid,
            fingerprint,
        }
    }

    /// 获取指纹字符串（用于发送到远程）
    pub fn as_str(&self) -> &str {
        &self.fingerprint
    }
}

/// 获取主 MAC 地址
fn get_mac_address() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        get_mac_address_macos()
    }
    #[cfg(target_os = "linux")]
    {
        get_mac_address_linux()
    }
    #[cfg(target_os = "windows")]
    {
        get_mac_address_windows()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        None
    }
}

#[cfg(target_os = "macos")]
fn get_mac_address_macos() -> Option<String> {
    use std::process::Command;

    let output = Command::new("networksetup")
        .args(["-listallhardwareports"])
        .output()
        .ok()?;

    let stdout = String::from_utf8_lossy(&output.stdout);

    // 查找第一个非空的 MAC 地址
    for line in stdout.lines() {
        if line.starts_with("Ethernet Address:") || line.starts_with("MAC Address:") {
            let parts: Vec<&str> = line.split(':').collect();
            if parts.len() >= 2 {
                let mac = parts[1..].join(":").trim().to_string();
                if !mac.is_empty() && mac != "00:00:00:00:00:00" {
                    return Some(mac);
                }
            }
        }
    }

    // 备选方案：使用 ifconfig
    let output = Command::new("ifconfig")
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    for line in stdout.lines() {
        if line.contains("ether ") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            for (i, part) in parts.iter().enumerate() {
                if *part == "ether" && i + 1 < parts.len() {
                    let mac = parts[i + 1].to_string();
                    if mac != "00:00:00:00:00:00" {
                        return Some(mac);
                    }
                }
            }
        }
    }

    None
}

#[cfg(target_os = "linux")]
fn get_mac_address_linux() -> Option<String> {
    use std::fs;

    // 尝试读取 /sys/class/net/*/address
    if let Ok(entries) = fs::read_dir("/sys/class/net") {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();

            // 跳过 lo 和 docker 等虚拟接口
            if name_str == "lo" || name_str.starts_with("docker") || name_str.starts_with("veth") {
                continue;
            }

            let path = entry.path().join("address");
            if let Ok(mac) = fs::read_to_string(&path) {
                let mac = mac.trim().to_string();
                if !mac.is_empty() && mac != "00:00:00:00:00:00" {
                    return Some(mac);
                }
            }
        }
    }

    None
}

#[cfg(target_os = "windows")]
fn get_mac_address_windows() -> Option<String> {
    use std::process::Command;

    let output = Command::new("getmac")
        .args(["/FO", "CSV", "/NH"])
        .output()
        .ok()?;

    let stdout = String::from_utf8_lossy(&output.stdout);

    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // 格式: "MAC Address","Transport Name"
        // 例如: "00-11-22-33-44-55","\Device\Tcpip_..."
        let parts: Vec<&str> = line.split(',').collect();
        if !parts.is_empty() {
            let mac = parts[0].trim_matches('"').replace('-', ':');
            if !mac.is_empty() && mac != "00:00:00:00:00:00" {
                return Some(mac);
            }
        }
    }

    None
}

/// 获取硬盘序列号
fn get_disk_serial() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        get_disk_serial_macos()
    }
    #[cfg(target_os = "linux")]
    {
        get_disk_serial_linux()
    }
    #[cfg(target_os = "windows")]
    {
        get_disk_serial_windows()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        None
    }
}

#[cfg(target_os = "macos")]
fn get_disk_serial_macos() -> Option<String> {
    use std::process::Command;

    // 使用 diskutil 获取主磁盘信息
    let output = Command::new("diskutil")
        .args(["info", "disk0"])
        .output()
        .ok()?;

    let stdout = String::from_utf8_lossy(&output.stdout);

    for line in stdout.lines() {
        if line.contains("Volume UUID:") || line.contains("Disk / Partition UUID:") {
            let parts: Vec<&str> = line.split(':').collect();
            if parts.len() >= 2 {
                let uuid = parts[1].trim().to_string();
                if !uuid.is_empty() {
                    return Some(uuid);
                }
            }
        }
    }

    // 备选：使用 ioreg 获取
    let output = Command::new("ioreg")
        .args(["-rd1", "-c", "IOMedia"])
        .output()
        .ok()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if line.contains("\"UUID\" =") {
            let parts: Vec<&str> = line.split('=').collect();
            if parts.len() >= 2 {
                let uuid = parts[1].trim().trim_matches('"').to_string();
                if !uuid.is_empty() {
                    return Some(uuid);
                }
            }
        }
    }

    None
}

#[cfg(target_os = "linux")]
fn get_disk_serial_linux() -> Option<String> {
    use std::fs;

    // 尝试读取 /sys/block/sda/serial 或 /sys/block/nvme0n1/serial
    let paths = [
        "/sys/block/sda/serial",
        "/sys/block/nvme0n1/serial",
        "/sys/block/vda/serial",
    ];

    for path in &paths {
        if let Ok(serial) = fs::read_to_string(path) {
            let serial = serial.trim().to_string();
            if !serial.is_empty() {
                return Some(serial);
            }
        }
    }

    // 备选：使用 lsblk
    use std::process::Command;
    let output = Command::new("lsblk")
        .args(["-d", "-o", "SERIAL", "-n"])
        .output()
        .ok()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let line = line.trim();
        if !line.is_empty() {
            return Some(line.to_string());
        }
    }

    None
}

#[cfg(target_os = "windows")]
fn get_disk_serial_windows() -> Option<String> {
    use std::process::Command;

    let output = Command::new("wmic")
        .args(["diskdrive", "get", "serialnumber"])
        .output()
        .ok()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();

    // 跳过标题行
    if lines.len() > 1 {
        let serial = lines[1].trim().to_string();
        if !serial.is_empty() {
            return Some(serial);
        }
    }

    None
}

/// 获取主板 UUID
fn get_motherboard_uuid() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        get_motherboard_uuid_macos()
    }
    #[cfg(target_os = "linux")]
    {
        get_motherboard_uuid_linux()
    }
    #[cfg(target_os = "windows")]
    {
        get_motherboard_uuid_windows()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        None
    }
}

#[cfg(target_os = "macos")]
fn get_motherboard_uuid_macos() -> Option<String> {
    use std::process::Command;

    let output = Command::new("ioreg")
        .args(["-rd1", "-c", "IOPlatformExpertDevice"])
        .output()
        .ok()?;

    let stdout = String::from_utf8_lossy(&output.stdout);

    for line in stdout.lines() {
        if line.contains("\"IOPlatformUUID\" =") {
            let parts: Vec<&str> = line.split('=').collect();
            if parts.len() >= 2 {
                let uuid = parts[1].trim().trim_matches('"').to_string();
                if !uuid.is_empty() {
                    return Some(uuid);
                }
            }
        }
    }

    None
}

#[cfg(target_os = "linux")]
fn get_motherboard_uuid_linux() -> Option<String> {
    use std::fs;

    // 尝试读取 DMI 信息
    let paths = [
        "/sys/class/dmi/id/product_uuid",
        "/sys/class/dmi/id/board_serial",
        "/etc/machine-id",
    ];

    for path in &paths {
        if let Ok(uuid) = fs::read_to_string(path) {
            let uuid = uuid.trim().to_string();
            if !uuid.is_empty() {
                return Some(uuid);
            }
        }
    }

    None
}

#[cfg(target_os = "windows")]
fn get_motherboard_uuid_windows() -> Option<String> {
    use std::process::Command;

    let output = Command::new("wmic")
        .args(["csproduct", "get", "uuid"])
        .output()
        .ok()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();

    // 跳过标题行
    if lines.len() > 1 {
        let uuid = lines[1].trim().to_string();
        if !uuid.is_empty() && uuid != "FFFFFFFF-FFFF-FFFF-FFFF-FFFFFFFFFFFF" {
            return Some(uuid);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_fingerprint() {
        let fp = HardwareFingerprint::generate();
        println!("MAC: {:?}", fp.mac_address);
        println!("Disk: {:?}", fp.disk_serial);
        println!("UUID: {:?}", fp.motherboard_uuid);
        println!("Fingerprint: {}", fp.fingerprint);

        // 指纹应该是 64 字符的十六进制字符串（SHA256）
        assert!(!fp.fingerprint.is_empty());
    }

    #[test]
    fn test_fingerprint_stability() {
        // 多次生成应该得到相同的指纹
        let fp1 = HardwareFingerprint::generate();
        let fp2 = HardwareFingerprint::generate();
        assert_eq!(fp1.fingerprint, fp2.fingerprint);
    }
}
