# Aginx ACP 方案C 实现文档

## 状态

✅ 已实现并编译通过

## 实现摘要

### 1. 依赖

- `portable-pty = "0.8"` - 跨平台 PTY 实现（替代原计划的 tokio-pty）

### 2. 核心组件

#### streaming.rs

**新增结构:**
```rust
pub struct StreamingSession {
    pub session_id: String,
    pub agent_info: AgentInfo,
    pub workdir: Option<String>,
    claude_session_uuid: String,
    tool_call_counter: u32,
    pty_slave: Option<Box<dyn Child + Send + Sync>>,
    pty_master: Option<Box<dyn MasterPty + Send>>,
}

pub struct StreamingPermissionRequest {
    pub request_id: String,
    pub description: String,
    pub tool_call: Option<StreamingToolCallInfo>,
    pub options: Vec<PermissionOption>,
}

pub enum StreamingResult {
    Completed(StopReason),
    PermissionNeeded { request: StreamingPermissionRequest, ... },
    Error(String),
}

pub enum StreamingAction {
    Stop(StopReason),
    PermissionNeeded(StreamingPermissionRequest),
}
```

**关键方法:**
```rust
// 带权限通道的交互式提示处理
pub fn send_prompt_interactive_with_permission<F>(
    &mut self,
    message: &str,
    on_update: F,
    perm_rx: Receiver<usize>,  // 从 handler 接收用户选择
) -> StreamingResult

// 写入权限响应到 PTY
pub fn send_permission_response(&mut self, choice: usize) -> Result<(), String>
```

#### handler.rs

**新增:**
```rust
pub struct PermissionChoiceSender {
    pub tx: std::sync::mpsc::Sender<usize>,
}

pub struct AcpHandler {
    // ...
    pending_permissions: RwLock<HashMap<String, PermissionChoiceSender>>,
}

pub async fn handle_prompt_streaming(&self, request: AcpRequest, tx: mpsc::Sender<String>) -> AcpResponse
pub async fn handle_permission_response(&self, request: AcpRequest) -> AcpResponse
```

### 3. 实际通信流程

```
App                                        Aginx                                Claude CLI
 |                                           |                                        |
 |── initialize ────────────────────────────>|                                        |
 |<─ InitializeResult ───────────────────────|                                        |
 |                                           |                                        |
 |── newSession ────────────────────────────>|                                        |
 |<─ { sessionId } ──────────────────────────|                                        |
 |                                           |── 启动 PTY 进程 ───────────────────────>|
 |                                           |                                        |
 |── prompt ────────────────────────────────>|                                        |
 |                                           |── 写入 prompt ────────────────────────>|
 |                                           |                                        |
 |                                           |<─ JSON 流(tool_use) ───────────────────|
 |                                           |                                        |
 |                                           |── 检测到需要权限 ──────────────────────|
 |                                           |                                        |
 |<─ requestPermission 通知 ─────────────────|                                        |
 |                                           |                                        |
 |── permissionResponse ────────────────────>|                                        |
 |                                           |── 写入数字到 PTY stdin ───────────────>|
 |                                           |                                        |
 |                                           |<─ JSON 流(结果) ───────────────────────|
 |                                           |                                        |
 |<─ 流式输出 (sessionUpdate) ───────────────|                                        |
 |<─ prompt response ───────────────────────|                                        |
```

### 4. 实际接口

**requestPermission 通知** (aginx → App):
```json
{
  "jsonrpc": "2.0",
  "method": "requestPermission",
  "params": {
    "requestId": "perm_uuid",
    "description": "Allow Edit(file.txt)?",
    "toolCall": {
      "toolCallId": "tc_session_1",
      "title": "Edit(file.txt)",
      "_meta": { "toolName": "Edit" }
    },
    "options": [
      { "optionId": "1", "label": "Allow this time", "kind": "allow_once" },
      { "optionId": "2", "label": "Always allow", "kind": "allow_always" },
      { "optionId": "3", "label": "Deny this time", "kind": "reject_once" },
      { "optionId": "4", "label": "Always deny", "kind": "reject_always" }
    ]
  }
}
```

**permissionResponse** (App → aginx):
```json
{
  "jsonrpc": "2.0",
  "id": "123",
  "method": "permissionResponse",
  "params": {
    "sessionId": "session-id",
    "outcome": {
      "outcome": "selected",
      "optionId": "1"
    }
  }
}
```

或使用 camelCase 字段名:
```json
{
  "params": {
    "sessionId": "session-id",
    "outcome": {
      "outcome": "selected",
      "optionId": "1"
    }
  }
}
```

**prompt 响应** (权限需要时):
```json
{
  "jsonrpc": "2.0",
  "id": "123",
  "result": {
    "stopReason": "permission_required",
    "permissionRequest": {
      "requestId": "perm_uuid",
      "description": "Allow Edit(file.txt)?",
      "options": [...]
    }
  }
}
```

### 5. 工具检测逻辑

从 tool_use 事件的元数据中提取工具名:

```rust
fn resolve_tool_name(&self, meta, raw_input, default) -> Option<String> {
    // 1. 从 _meta.toolName / _meta.tool_name / _meta.name
    if let Some(meta) = meta {
        for key in &["toolName", "tool_name", "name"] {
            if let Some(name) = meta.get(key).and_then(|v| v.as_str()) {
                return Some(name.to_string());
            }
        }
    }

    // 2. 从 rawInput.tool / rawInput.toolName / rawInput.tool_name
    if let Some(raw) = raw_input {
        for key in &["tool", "toolName", "tool_name", "name"] {
            if let Some(name) = raw.get(key).and_then(|v| v.as_str()) {
                return Some(name.to_string());
            }
        }
    }

    // 3. 使用默认值
    Some(default.to_string())
}
```

### 6. 权限决策

```rust
fn tool_requires_permission(&self, tool_call: &StreamingToolCallInfo) -> bool {
    match tool_call.tool_name.as_deref() {
        Some("read" | "Read" | "NBRead") => false,  // 读取操作通常安全
        Some("glob" | "Glob") => false,
        Some("grep" | "Grep") => false,
        Some("edit" | "Edit" | "NBEdit") => true,
        Some("write" | "Write" | "NBWrite") => true,
        Some("delete" | "Delete") => true,
        Some("bash" | "Bash") => true,
        _ => true,  // 默认需要权限
    }
}
```

## 测试方法

### 本地测试

```bash
# 运行 aginx
./target/release/aginx --verbose

# 在另一个终端，使用测试客户端
cargo run --example test_session agent://localhost:8080
```

### 部署到服务器

```bash
# 同步并构建
rsync -avz --exclude 'target' --exclude '.git' ./ root@111.170.156.43:/data/www/aginx/
ssh root@111.170.156.43 "cd /data/www/aginx && cargo build --release"

# 重启服务
ssh root@111.170.156.43 "pkill -f 'target/release/aginx' ; nohup /data/www/aginx/target/release/aginx > /data/www/aginx/aginx.log 2>&1 &"

# 查看日志
ssh root@111.170.156.43 "tail -f /data/www/aginx/aginx.log"
```

## 与 App 端集成

App 端需要确认:

1. **使用 ACP 协议**: JSON-RPC 2.0 over NDJSON (newline-delimited JSON)
2. **方法名**: `permissionResponse` (不是 `respondPermission`)
3. **字段名**: 使用 camelCase (`sessionId`, `optionId`, `toolCallId`)
4. **outcome 格式**: `{ "outcome": "selected", "optionId": "1" }`

App 端代码示例:
```kotlin
// 发送权限响应
fun sendPermissionResponse(sessionId: String, optionId: String) {
    val request = AcpRequest(
        method = "permissionResponse",
        params = mapOf(
            "sessionId" to sessionId,
            "outcome" to mapOf(
                "outcome" to "selected",
                "optionId" to optionId
            )
        )
    )
    sendRequest(request)
}
```

## 差异总结

| 原方案 | 实际实现 |
|--------|----------|
| tokio-pty | portable-pty |
| TCP 自定义协议 | ACP (JSON-RPC over NDJSON) |
| respondPermission | permissionResponse |
| choice: Int | optionId: String |
| 独立的 createSession/sendMessage | newSession/prompt |

## 文件变更

- `Cargo.toml` - 添加 portable-pty 依赖
- `src/acp/streaming.rs` - 完全重构，添加 PTY 支持
- `src/acp/handler.rs` - 添加权限处理逻辑
