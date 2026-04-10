# Aginx 代码问题清单

## 1. 设备身份与注册机制（严重 - 安全）

### 1.1 问题描述

当前 aginx_id 是随机分配的，没有与硬件绑定，存在以下问题：
- 重置 aginx 后 ID 会变
- 别人知道 ID 后可以假冒设备

### 1.2 期望行为

```
┌─────────────────────────────────────────────────────────────────┐
│                        注册流程                                  │
│                                                                 │
│  首次启动:                                                      │
│  设备 → 计算硬件指纹 → 发送到远程 → 远程分配 aginx_id            │
│                        → 保存 {指纹: aginx_id} 映射              │
│                        → 返回 aginx_id → 设备保存到本地           │
│                                                                 │
│  重置后启动:                                                    │
│  设备 → 计算硬件指纹 → 发送到远程 → 远程发现指纹已存在            │
│                        → 返回旧的 aginx_id（不变！）             │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

### 1.3 当前缺失

- [ ] 设备端没有计算硬件指纹
- [ ] 没有向远程注册硬件指纹
- [ ] aginx_id 是随机分配的，没有和硬件指纹绑定
- [ ] 重置后 aginx_id 会变

### 1.4 需要实现

1. **生成硬件指纹**（MAC + 硬盘序列号 + 主板UUID 等）
2. **注册时发送硬件指纹到远程**
3. **远程根据硬件指纹返回/分配 aginx_id**

---

## 2. App 绑定机制（严重 - 安全）

### 2.1 配对码验证漏洞

**文件**: `src/binding/mod.rs:104-126`

```rust
// save_pair_code - 只保存时间戳，没保存 code！
fn save_pair_code(&self, code: &str) -> anyhow::Result<()> {
    let data = PairCodeData {
        created_at: chrono::Utc::now().timestamp(),  // code 被忽略了！
    };
}

// load_pair_code - code 参数完全没用！
fn load_pair_code(&self, code: &str) -> Option<PairCodeData> {
    // code 参数被忽略，只检查文件存在和时间戳
    if now - data.created_at > PAIR_CODE_TTL_SECS {
        return None;
    }
    Some(data)  // 任何 code 都返回 Some
}
```

**结果**：在配对码有效期内（5分钟），**任何 6 位数字都能通过验证**！

### 2.2 绑定关系错误

**期望**：一个 aginx 只能被一个 App 绑定（独占模式）
**当前**：`bindings.json` 是数组，存储了多个 App

### 2.3 需要修复

- [ ] 配对码保存和验证实际的 code 值
- [ ] 一个 aginx 只能绑定一个 App（独占）
- [ ] 再次绑定需要先解绑旧的

---

## 3. BindingManager 多实例问题（高 - 状态不一致）

### 3.1 问题描述

每处都 `new` 一个新的 BindingManager 实例，状态不共享：

| 位置 | 文件 | 行号 |
|------|------|------|
| main.rs | pair 命令 | 266 |
| main.rs | devices 命令 | 277 |
| main.rs | unbind 命令 | 297 |
| relay/mod.rs | bindDevice | 670 |
| acp/handler.rs | ACP handler | 41 |
| server/tcp.rs | TCP server | 27 |

### 3.2 问题

- CLI 命令生成的配对码，ACP handler 无法验证（不同实例）
- 各处状态不同步

### 3.3 建议

- BindingManager 应该是单例
- 或者所有地方都通过文件读写共享状态

---

## 4. 代码重复问题（中 - 维护困难）

### 4.1 bindDevice 逻辑重复

| 位置 | 说明 |
|------|------|
| `src/relay/mod.rs:670-684` | relay 模式的 bindDevice 处理 |
| `src/acp/handler.rs:593-630` | ACP handler 的 bindDevice 处理 |

两处都实现了相同的 `verify_pair_code` 逻辑。

### 4.2 流式处理重复

| 位置 | 说明 |
|------|------|
| `src/server/handler.rs:92-132` | TCP server 的流式处理 |
| `src/relay/mod.rs:346-428` | relay 的流式处理 |
| `src/acp/handler.rs:287-439` | ACP handler 的流式处理 |

三处都有类似的 `handle_prompt_streaming` 和通知转发逻辑。

### 4.3 send_message 重复

| 位置 | 说明 |
|------|------|
| `src/agent/manager.rs:189-243` | AgentManager 的 send_message |
| `src/agent/session.rs:340-360` | Session 的 send_message |
| `src/relay/mod.rs:575-615` | relay 的 send_message 处理 |

无状态和有状态两种调用方式混用。

---

## 5. 错误处理问题（中）

### 5.1 大量错误被忽略（let _ =）

**文件**: 多处

```rust
let _ = fs::create_dir_all(&data_dir);      // binding/mod.rs:61
let _ = fs::remove_file(self.pair_code_path());  // binding/mod.rs:130
let _ = self.save_pair_code(&code);         // binding/mod.rs:139
let _ = self.save_devices();                // binding/mod.rs:168, 184, 200
let _ = notify_handle.await;                // relay/mod.rs:411
let _ = w.write_all(json.as_bytes()).await; // acp/handler.rs:685-687
```

**问题**: 关键操作失败时没有错误处理，可能导致数据丢失或状态不一致。

### 5.2 unwrap 可能导致 panic

**文件**: 多处

```rust
src/bin/relay.rs:292:   tx.send(serde_json::to_string(&pure_response).unwrap())
src/config/loader.rs:119: toml::to_string_pretty(&config).unwrap()
src/acp/stream.rs:90:   StreamEvent::from_line(json).unwrap()
```

### 5.3 expect 在生产代码中

**文件**: `src/server/tcp.rs:49`

```rust
.expect("Invalid address");
```

---

## 6. 未实现的功能（中）

### 6.1 TODO 注释

| 文件 | 行号 | 说明 |
|------|------|------|
| acp/handler.rs | 174 | session loading/resumption 未实现 |
| acp/handler.rs | 449 | cancellation 未实现 |

### 6.2 loadSession 只返回成功

**文件**: `src/acp/handler.rs:154-175`

```rust
async fn handle_load_session(&self, request: AcpRequest) -> AcpResponse {
    // For now, just return success with the session ID
    // TODO: Implement actual session loading/resumption
    AcpResponse::success(...)
}
```

---

## 7. 硬编码问题（低 - 灵活性）

### 7.1 Claude 特定代码

**文件**: 多处

```rust
// src/acp/streaming.rs:20
enum ClaudeEvent { ... }  // 应该是通用的 AgentEvent

// src/acp/streaming.rs:208
.map_err(|e| format!("Failed to spawn claude: {}", e))?;  // 错误信息写死 claude

// src/acp/streaming.rs:231
tracing::debug!("Claude JSON line: {}", line);  // 日志写死 Claude

// src/agent/session.rs:444
Err(format!("Claude error: {}", stderr))  // 错误信息写死 Claude
```

### 7.2 默认值硬编码

```rust
// src/main.rs:110
#[arg(short = 'a', long, default_value = "claude")]

// src/relay/mod.rs:595
.unwrap_or("claude");

// src/acp/handler.rs:134
.unwrap_or_else(|| "claude".to_string());
```

### 7.3 配置未使用

| 配置字段 | 说明 |
|----------|------|
| env_remove | 在部分地方使用，但 manager.rs:223 仍硬编码 `CLAUDECODE` |
| session_args | 在 session.rs 中使用，但其他地方可能遗漏 |

---

## 8. 架构问题（低）

### 8.1 bin/relay.rs 独立程序

存在 `src/bin/relay.rs` 作为独立的 relay 服务器程序，但与主程序 `aginx` 的功能有重叠。

### 8.2 模块职责不清

- `acp/handler.rs` 和 `relay/mod.rs` 都处理 ACP 请求
- `server/handler.rs` 也处理类似逻辑
- 三处代码高度相似但各自维护

### 8.3 stream.rs vs streaming.rs

`src/acp/` 目录下有两个文件：
- `stream.rs` (4KB)
- `streaming.rs` (35KB)

功能重叠，命名混淆。

---

## 9. 其他问题

### 9.1 权限自动通过

**文件**: `src/acp/streaming.rs:244-247`

```rust
// In this simplified version, we auto-allow for now
// In production, this would pause and wait for user response
tracing::info!("Permission needed for {} - auto-allowing for now", perm.description);
```

权限请求被自动通过，没有等待用户响应。

### 9.2 缺少输入验证

- `deviceName` 参数没有长度限制
- `pairCode` 没有格式验证
- 路径参数没有防止路径遍历

### 9.3 日志语言混用

代码中同时使用中文和英文日志：
- `src/agent/manager.rs:242`: "Claude 响应"
- `src/agent/session.rs:242`: "会话创建成功"

---

## 10. 优先级排序

| 优先级 | 问题 | 严重程度 | 影响 |
|--------|------|----------|------|
| 1 | 设备身份与注册机制（硬件指纹） | 严重 | 安全漏洞 |
| 2 | 配对码验证漏洞（任何code都能通过） | 严重 | 安全漏洞 |
| 3 | 绑定关系（一个aginx只能绑定一个App） | 严重 | 功能错误 |
| 4 | BindingManager 多实例 | 高 | 状态不一致 |
| 5 | 错误处理被忽略 | 中 | 数据丢失风险 |
| 6 | 代码重复（3处流式处理、bindDevice） | 中 | 维护困难 |
| 7 | 未实现的功能（loadSession、cancel） | 中 | 功能不完整 |
| 8 | 硬编码问题 | 低 | 灵活性差 |
| 9 | 权限自动通过 | 低 | 用户体验 |
| 10 | 日志语言混用 | 低 | 可读性 |
