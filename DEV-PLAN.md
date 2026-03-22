# aginx 开发计划

> **版本**: v0.2.0
> **日期**: 2026-03-22
> **状态**: 规划中

---

## 1. 设计变更概述

### 1.1 核心变更

| 项目 | 旧设计 | 新设计 |
|------|--------|--------|
| 运行模式判断 | `relay.enabled` | `server.mode` |
| Relay 连接方式 | WebSocket | **纯 TCP** |
| Relay 地址格式 | `wss://relay.yinnho.cn` | `{id}.relay.yinnho.cn:8600` |
| 心跳机制 | WebSocket Ping/Pong | 应用层 `{"type":"ping"}` |

### 1.2 配置格式变更

**旧配置**:
```toml
[server]
port = 86

[relay]
enabled = true
server = "wss://relay.yinnho.cn"
id = "abc123"
```

**新配置**:
```toml
[server]
mode = "relay"  # direct | relay
port = 86

[relay]
url = "abc123.relay.yinnho.cn:8600"  # 完整地址
```

---

## 2. 需要修改的文件

### 2.1 src/config/mod.rs - 配置结构

**当前问题**:
```rust
pub struct RelayConfig {
    pub enabled: bool,
    pub server: Option<String>,
    pub id: Option<String>,
}
```

**修改为**:
```rust
/// 运行模式
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ServerMode {
    /// 直连模式 - 监听本地 TCP 86
    #[default]
    Direct,
    /// 中继模式 - 连接 relay 服务器
    Relay,
}

/// 服务器配置
pub struct ServerConfig {
    /// 运行模式
    #[serde(default)]
    pub mode: ServerMode,
    // ... 其他字段保持不变
}

/// 中继配置
pub struct RelayConfig {
    /// Relay 完整地址: {id}.relay.yinnho.cn:8600
    pub url: String,

    /// 心跳间隔 (秒)
    #[serde(default = "default_heartbeat_interval")]
    pub heartbeat_interval: u64,
}

/// 直连配置
pub struct DirectConfig {
    /// 公网访问地址
    pub public_url: Option<String>,
}
```

**修改内容**:
- [ ] 添加 `ServerMode` 枚举
- [ ] 在 `ServerConfig` 中添加 `mode` 字段
- [ ] 修改 `RelayConfig`：移除 `enabled`、`server`、`id`，添加 `url`
- [ ] 添加 `DirectConfig` 结构
- [ ] 更新 `Config` 结构，添加 `direct` 字段
- [ ] 更新 `Default` 实现

---

### 2.2 src/main.rs - 启动逻辑

**当前问题**:
```rust
#[arg(long, default_value = "wss://relay.yinnho.cn")]
relay_server: String,  // WebSocket 地址

// 启动逻辑
server.run().await?;
```

**修改为**:
```rust
/// 运行模式
#[arg(short = 'm', long, value_enum)]
mode: Option<ServerMode>,

/// Relay 完整地址 (mode=relay 时使用)
#[arg(long, value_name = "URL")]
relay_url: Option<String>,

// 启动逻辑
match config.server.mode {
    ServerMode::Direct => {
        tracing::info!("运行模式: 直连");
        // 监听本地 TCP 86
        server.run().await?;
    }
    ServerMode::Relay => {
        tracing::info!("运行模式: 中继");
        // 连接 relay，不监听本地端口对外服务
        let mut relay_client = relay::RelayClient::new(&config);
        relay_client.connect(Arc::new(config)).await?;
    }
}
```

**修改内容**:
- [ ] 修改命令行参数：`--mode` 替代 `--relay`
- [ ] 修改 `relay_url` 参数格式
- [ ] 修改 `apply_args` 函数
- [ ] 修改启动逻辑，根据 mode 分支
- [ ] 更新 `print_startup_info` 函数

---

### 2.3 src/relay/mod.rs - Relay 客户端 (重写)

**当前问题**: 使用 WebSocket 连接
```rust
let (ws, _) = tokio_tungstenite::connect_async(&self.server_url).await?;
// ...
ws_sink.send(Message::Ping(vec![])).await
```

**修改为**: 纯 TCP 连接
```rust
use tokio::net::TcpStream;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub struct RelayClient {
    /// Relay 完整地址
    relay_url: String,
}

impl RelayClient {
    pub fn new(config: &Config) -> Self {
        Self {
            relay_url: config.relay.url.clone(),
        }
    }

    pub async fn connect(&mut self, config: Arc<Config>) -> anyhow::Result<()> {
        tracing::info!("连接 Relay: {}", self.relay_url);

        // 纯 TCP 连接
        let stream = TcpStream::connect(&self.relay_url).await?;
        let (reader, mut writer) = stream.into_split();

        // 注册到 relay
        let register = serde_json::json!({"type": "register"});
        writer.write_all(format!("{}\n", register).as_bytes()).await?;

        // 启动心跳任务
        // 启动消息接收任务
        // ...
    }
}
```

**修改内容**:
- [ ] 移除 WebSocket 依赖 (`tokio_tungstenite`)
- [ ] 使用纯 `TcpStream`
- [ ] 实现应用层心跳: `{"type":"ping"}` / `{"type":"pong"}`
- [ ] 实现消息读写（行分隔 JSON）
- [ ] 移除 `WsSink`、`WsStream` 类型别名

---

### 2.4 src/config/loader.rs - 配置加载

**修改内容**:
- [ ] 更新 `CliArgs` 结构
- [ ] 更新 `apply_cli_args` 函数
- [ ] 处理新配置格式

---

### 2.5 src/server/handler.rs - 请求处理

**检查内容**:
- [ ] 确认 `relayUrl` 字段使用正确的地址格式
- [ ] 确保与 relay 模式兼容

---

## 3. 协议定义

### 3.1 Relay 内部消息格式 (aginx ↔ relay)

```json
// 注册
{"type": "register"}→ {"type": "registered", "id": "xxx", "url": "agent://xxx.relay.yinnho.cn"}

// 心跳
{"type": "ping"}→ {"type": "pong"}

// 数据传输
{"type": "data", "clientId": "c_xxx", "data": {...}}
← {"clientId": "c_xxx", "result": {...}}
```

### 3.2 消息分隔

- 每条消息以 `\n` 结尾
- JSON 格式

---

## 4. 依赖变更

### 4.1 需要移除的依赖

```toml
# 移除
tokio-tungstenite = "0.21"
```

### 4.2 需要添加的依赖

无（纯 TCP 使用 tokio 内置）

---

## 5. 测试计划

### 5.1 单元测试

- [ ] `ServerMode` 枚举解析测试
- [ ] 配置文件解析测试
- [ ] Relay 消息格式测试

### 5.2 集成测试

- [ ] 直连模式测试
- [ ] Relay 模式测试
- [ ] 心跳机制测试
- [ ] 断线重连测试

---

## 6. 实施顺序

| 步骤 | 文件 | 内容 | 状态 |
|------|------|------|------|
| 1 | `src/config/mod.rs` | 更新配置结构 | ✅ 完成 |
| 2 | `src/config/loader.rs` | 更新配置加载 | ✅ 完成 |
| 3 | `src/relay/mod.rs` | 重写为纯 TCP | ✅ 完成 |
| 4 | `src/main.rs` | 更新启动逻辑 | ✅ 完成 |
| 5 | `src/server/handler.rs` | 检查兼容性 | ✅ 完成 |
| 6 | 编译检查 | cargo build | ✅ 通过 |
| 7 | 测试 | 单元测试 + 集成测试 | 待开始 |

---

## 7. 已完成的修改摘要

### 7.1 配置迁移

提供迁移脚本或自动检测旧配置格式并提示用户更新。

### 7.2 命令行参数

- `--relay` → `--mode relay`
- `--relay-server` → `--relay-url`

---

**最后更新**: 2026-03-22
