# Aginx 开发文档

## 项目定位

Aginx 是 **Agent 互联网** 基础设施。aginx 本身是纯消息路由服务器（类比 nginx），不涉及 LLM 配置、模型选择等。LLM 是 Agent 自己的事。

## 项目结构

```
aginx/                    # 仓库根目录
├── aginx/                # Agent 网关 (Rust)
│   └── src/
│       ├── main.rs           # 入口，CLI 参数解析
│       ├── config/           # 配置加载和管理
│       │   ├── mod.rs            # Config、AgentEntry、StorageConfig 等结构
│       │   └── loader.rs        # 配置文件读取 + CLI 参数覆盖
│       ├── agent/            # Agent 管理
│       │   ├── mod.rs            # 模块导出
│       │   ├── manager.rs        # AgentManager + AgentInfo（运行时）
│       │   ├── discovery.rs      # AgentConfig（aginx.toml 格式）+ 目录扫描
│       │   └── session.rs        # SessionManager + Session + 持久化
│       ├── acp/              # ACP 协议实现
│       │   ├── mod.rs            # 模块导出
│       │   ├── types.rs          # ACP 类型定义（请求/响应/通知）
│       │   ├── handler.rs        # AcpHandler — 请求路由和业务逻辑
│       │   ├── streaming.rs      # Legacy 流式输出（每次 prompt 启新进程）
│       │   ├── agent_process.rs  # AcpAgentProcess — 持久进程模式
│       │   ├── claude_event.rs   # 共享类型 + 通知构建 + 工具辅助函数
│       │   └── tests.rs          # 测试
│       ├── server/           # TCP 直连服务器
│       │   ├── tcp.rs            # TCP 监听 + 连接限制
│       │   └── handler.rs        # 单连接 JSON-RPC 读写循环
│       ├── relay/            # Relay 客户端
│       │   └── mod.rs            # TCP 连接 relay、注册、心跳、消息转发
│       ├── binding/          # 设备绑定（配对码）
│       ├── auth/             # 认证（未实现）
│       ├── fingerprint/      # 硬件指纹（内存生成，不持久化）
│       └── bin/
│           └── relay.rs      # relay-server 独立 binary（未完成）
├── aginx-api/            # 云端 API (Rust/Axum) — 注册、认证
├── aginx-relay/          # Relay 中继服务 (Rust) — 独立部署
├── aginxium/             # 客户端引擎 (Rust) — 连接、会话、事件管理
├── aginx-controller/     # Android 控制器 (Kotlin) — 已废弃，用 aginxium 替代
└── aginx-app/            # Android App (Kotlin) — 已废弃，用 aginxium 替代
```

## 服务器信息

- **SSH**: `ssh 86quan` (ubuntu@106.75.32.216)
- **服务端目录**: `/data/www/aginx/` | `/data/www/aginx-api/` | `/data/www/aginx-relay/`
- **域名**: `api.aginx.net` → API, `relay.aginx.net` → Relay

## 核心设计

### 零配置启动
- 服务器端无需预先配置，注册即用
- 所有配置通过客户端（App/CLI）完成
- 配置保存在 `~/.aginx/` 目录

### 连接模式
- **Direct 模式**: 监听本地 TCP 端口（默认 86），客户端直连
- **Relay 模式**: 主动连接 relay 服务器（默认 `relay.aginx.net:8600`），NAT 穿透
- 两种模式都使用 ACP 协议（JSON-RPC 2.0 over TCP, ndjson 格式）

### Agent 类型

| 类型 | 说明 | 实现方式 |
|------|------|----------|
| `claude` | Claude CLI | AcpAgentProcess 持久进程 或 legacy streaming |
| `process` | 通用 CLI | stdin/stdout 进程调用 |
| `builtin` | 内置 | 内存处理，包括 echo、info、shell |

### Agent 发现
- 每个 agent 放置 `aginx.toml` 配置文件
- 扫描目录: `~/.aginx/agents/`
- `discoverAgents` → 扫描 → `registerAgent` → 注册

### 设备绑定
- 生成配对码 → App 输入配对码 → `bindDevice` 绑定
- 命令: `aginx pair` 生成, `aginx devices` 查看

## ACP 协议

版本 0.15.0，JSON-RPC 2.0，ndjson 传输（每行一个 JSON 对象）。

### 核心 ACP 方法

| 方法 | 方向 | 参数 | 返回 |
|------|------|------|------|
| `initialize` | req→resp | `{protocolVersion, clientInfo}` | `{protocolVersion, agentInfo, capabilities}` |
| `session/new` | req→resp | `{cwd?, _meta.agentId?}` | `{sessionId}` |
| `session/load` | req→resp | `{sessionId}` | `{sessionId, status}` |
| `session/prompt` | req→stream | `{sessionId, prompt:[{type,text}]}` | `{streaming:true}` + 通知 + 最终响应 |
| `session/cancel` | req→resp | `{sessionId}` | `{cancelled}` |

注意：`session/prompt` 不走普通 request/response，必须通过流式通道处理。非流式调用会返回错误 "Use streaming endpoint"。

方法别名：`session/load` = `loadSession`，`getMessages` = `getConversationMessages`

### 流式通知（server → client）

```json
{"method":"sessionUpdate","params":{"sessionId":"xxx","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"..."}}}}
```

```json
{"method":"sessionUpdate","params":{"sessionId":"xxx","update":{"sessionUpdate":"tool_call","toolCallId":"tc_1","title":"Read(file.rs)","status":"in_progress","kind":"Read"}}}
```

```json
{"method":"sessionUpdate","params":{"sessionId":"xxx","update":{"sessionUpdate":"tool_call_update","toolCallId":"tc_1","status":"completed"}}}
```

### 权限通知（server → client → server）

```json
{"method":"requestPermission","params":{"requestId":"req_1","description":"Allow writing to file.py?","options":[{"optionId":"1","label":"Allow once"}]}}
```

Client 回复: `{"jsonrpc":"2.0","id":"req_1","result":{"outcome":{"outcome":"selected","optionId":"1"}}}`

### Aginx 扩展方法

| 方法 | 参数 | 说明 |
|------|------|------|
| `listAgents` | — | 列出已注册 Agent |
| `discoverAgents` | `{path?, maxDepth?}` | 扫描目录发现新 Agent |
| `registerAgent` | `{configPath}` 或 `{agent}` | 注册 Agent |
| `listConversations` | `{agentId}` | 列出对话 |
| `getMessages` | `{sessionId, limit?}` | 获取消息历史（别名: `getConversationMessages`） |
| `deleteConversation` | `{sessionId}` | 删除对话 |
| `listSessions` | `{agentId?}` | 列出会话 |
| `listDirectory` | `{path}` | 浏览服务器目录（限 home 目录） |
| `readFile` | `{path}` | 读取服务器文件（base64, 限 10MB） |
| `bindDevice` | `{pairCode, deviceName}` | 绑定设备 |

## 配置

### 全局配置 `~/.aginx/config.toml`

```toml
[server]
mode = "relay"          # direct | relay
name = "aginx"
version = "0.1.0"
port = 86               # 本地监听端口（direct 模式）
host = "0.0.0.0"
access = "public"       # public | private
max_connections = 100
max_concurrent_sessions = 10
session_timeout_seconds = 1800

[relay]
id = "abc123"                              # Aginx ID（首次启动自动申请）
url = "abc123.relay.aginx.net:8600"        # 完整地址（优先于 id 构建）
heartbeat_interval = 30

[direct]
public_url = "agent://myserver.com"        # 公网访问地址（直连模式）

[auth]
jwt_secret = "your-secret-key"             # JWT 密钥（私有模式）

[agents]
list = []                # 主配置中的 agent 列表（通常为空，用 aginx.toml 管理）
```

Relay 地址优先级：`url` 字段 > 根据 `id` 构建 `{id}.relay.aginx.net:8600` > 默认 `relay.aginx.net:8600`

### Agent 配置 `~/.aginx/agents/<name>/aginx.toml`

```toml
id = "claude"
name = "Claude"
agent_type = "claude"
description = "Anthropic coding assistant"
version = "1.0"

[command]
path = "claude"
args = ["--print", "--output-format", "stream-json"]
env_remove = ["CLAUDECODE"]

[session]
require_workdir = true

  [session.resume]
  session_id_path = "env:CLAUDE_SESSION_ID"
  resume_args = ["--resume"]

[capabilities]
chat = true
code = true
ask = true
streaming = true
permissions = true

[detect]
check_command = "which"
check_args = ["claude"]

[permissions]
default_allowed = ["read", "search", "web_search", "memory_search"]
```

### 数据目录

```
~/.aginx/
├── config.toml              # 全局配置
├── binding.json             # 设备绑定数据
├── pair_code.json           # 临时配对码
├── failed_attempts.json     # 绑定失败记录
├── agents/                  # Agent 配置
│   └── claude/
│       └── aginx.toml
└── sessions/                # 会话元数据
    └── {agent_id}/
        └── {session_id}.json
```

Claude 类型的会话内容存储在 `~/.claude/projects/` 的 JSONL 文件中，aginx 元数据存在 `~/.aginx/sessions/` 中。

## 部署命令

```bash
# 同步代码到服务器
rsync -avz --exclude 'target' --exclude '.git' -e ssh ./ 86quan:/data/www/aginx/

# 编译
ssh 86quan "cd /data/www/aginx && cargo build --release"

# 重启
ssh 86quan "pkill -f aginx ; nohup /data/www/aginx/target/release/aginx > /data/www/aginx/aginx.log 2>&1 &"

# 查看日志
ssh 86quan "tail -f /data/www/aginx/aginx.log"
```

## 本地开发

```bash
# 运行（默认 relay 模式）
cargo run --bin aginx -- -v

# 直连模式
cargo run --bin aginx -- --mode direct --port 8866

# ACP stdio 模式（IDE 集成）
cargo run --bin aginx -- acp --stdio

# 测试连接
cargo run --example test_client_relay agent://rcs0aj94.relay.aginx.net
```

## Relay 协议

aginx ↔ relay 使用纯 TCP 长连接：

```
# 注册（aginx → relay）
→ {"type":"register","id":"<id>","fingerprint":"<fingerprint>"}
← {"type":"registered","id":"abc123","url":"agent://abc123.relay.aginx.net"}

# 心跳
→ {"type":"ping"}
← {"type":"pong"}

# 数据转发（relay 在客户端消息中注入 clientId，在 aginx 响应中移除）
→ {"type":"data","clientId":"c_xxx","data":{...}}
← {"clientId":"c_xxx","jsonrpc":"2.0","id":1,"result":{...}}

# 断开通知
← {"type":"disconnected","client_id":"c_xxx"}

# 错误
← {"type":"error","message":"..."}
```

App 看到的始终是纯 JSON-RPC，relay 包装层（clientId、type）对 App 透明。
