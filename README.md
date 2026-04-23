# Aginx

> Agent Protocol — 访问 Agent 就像访问网站一样简单。

## 一句话

**让世界上每一个 Agent 都有一个地址。**

```
agent://rcs0aj94.relay.aginx.net
```

一个 URL，直达 Agent。不需要知道 IP、端口、协议。

## 定位

Aginx 是 Agent 互联网的基础设施，每一层都对应互联网的一个组件：

| Aginx | 互联网 | 说明 |
|-------|--------|------|
| aginx | nginx | 消息路由器，把请求路由到 Agent |
| aginxium | Chromium | 统一客户端引擎 |
| aginx-controller | Chrome | 用户产品（App） |
| aginx-api | DNS / 注册中心 | 注册、发现、认证 |
| aginx-relay | CDN / 骨干网 | NAT 穿透、连接转发 |
| agent:// URL | https:// URL | 统一寻址 |

## 架构

```
┌─────────────┐                    ┌─────────────┐                    ┌─────────────┐
│   客户端     │─── agent://URL ───▶│    Relay    │──── TCP ──────────▶│   Aginx     │
│ (App/IDE)   │                    │  (中继服务器) │                    │  (Agent宿主) │
└─────────────┘                    └─────────────┘                    └──────┬──────┘
                                                                              │
                                                                              ▼
                                                                    ┌─────────────────┐
                                                                    │     Agents      │
                                                                    │  Claude / 自定义 │
                                                                    └─────────────────┘
```

### 两种连接模式

| 模式 | 说明 | 场景 |
|------|------|------|
| **Relay** | 通过中继服务器 | 内网、NAT 穿透 |
| **Direct** | 直接 TCP 监听 | 公网服务器 |

### 核心原则

1. **aginx 是纯消息路由器** — 不关心 Agent 内部用什么模型、什么语言
2. **任何 CLI 都可以是 Agent** — 通过 `aginx.toml` 配置即可接入
3. **agent:// 统一寻址** — 客户端只需一个 URL
4. **纯 TCP + JSON-RPC** — 不依赖 WebSocket，不依赖 SDK

## 项目结构

```
aginx/
├── aginx/            # Agent 网关 (Rust) - 消息路由、Agent 管理
├── aginx-api/        # 云端 API (Rust/Axum) - 注册、认证
├── aginx-relay/      # 中继服务 (Rust) - NAT 穿透、连接转发
├── aginxium/         # 客户端引擎 (Rust) - 协议、连接、会话管理
└── aginx-controller/ # 用户 App (Android) - 由 aginxium 驱动
```

## 快速开始

### 安装

**从 crates.io 安装**

```bash
cargo install aginx
```

**从 GitHub Release 下载**

到 [Releases](https://github.com/yinnho/aginx/releases) 页面下载对应平台的二进制。

**从源码编译**

```bash
cargo install --git https://github.com/yinnho/aginx
```

### 启动

```bash
# Relay 模式（默认）
aginx

# 直连模式
aginx --mode direct

# 指定端口
aginx --port 8866

# 调试日志
aginx -d
```

### Agent 配置

每个 Agent 放一个 `aginx.toml`：

```toml
# ~/.aginx/agents/claude/aginx.toml
id = "claude"
name = "Claude"
agent_type = "claude"
description = "Anthropic coding assistant"

[command]
path = "claude"
args = ["--print", "--output-format", "stream-json"]
env_remove = ["CLAUDECODE"]

[session]
require_workdir = true

[capabilities]
chat = true
code = true
```

### 发现和注册

```bash
# 扫描 ~/.aginx/agents/ 下的 aginx.toml
# 通过 JSON-RPC 方法注册：
# discoverAgents → 扫描 → registerAgent → 注册
```

## 协议

Aginx 使用极简的 **JSON-RPC 2.0 over TCP**（每行一个 JSON 对象，ndjson 格式）。

不是外部任何标准协议，就是简单的请求-响应：

```json
// 请求
{"jsonrpc":"2.0","id":1,"method":"prompt","params":{"agent":"claude","message":"hello"}}

// 流式通知
{"jsonrpc":"2.0","method":"chunk","params":{"text":"Hello"}}
{"jsonrpc":"2.0","method":"chunk","params":{"text":"!"}}

// 最终响应
{"jsonrpc":"2.0","id":1,"result":{"stopReason":"end_turn","sessionId":"sess_xxx"}}
```

### 核心方法

| 方法 | 说明 |
|------|------|
| `initialize` | 握手，可选携带 authToken 认证 |
| `prompt` | 发送消息（流式响应） |
| `listAgents` | 列出已注册 Agent |
| `ping` | 心跳 |
| `bindDevice` | 设备绑定（配对码） |

### 流式响应

`prompt` 返回通知推送文本片段，最后返回结果：

```json
{"jsonrpc":"2.0","method":"chunk","params":{"text":"..."}}
{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn","response":"..."}}
```

### 扩展方法

| 方法 | 说明 |
|------|------|
| `discoverAgents` | 扫描发现新 Agent |
| `registerAgent` | 注册 Agent |
| `listConversations` | 列出对话 |
| `getMessages` | 获取消息历史 |
| `deleteConversation` | 删除对话 |
| `listSessions` | 列出会话 |
| `listDirectory` | 浏览服务器目录 |
| `readFile` | 读取服务器文件 |

## 安全

### 设备绑定（配对码）

```bash
# 服务端生成配对码
aginx pair
# → Pair code: ABC123 (expires in 300s)

# App 输入配对码绑定
# bindDevice("ABC123", "Pixel 9") → 返回 token
```

- 配对码 6 位字母数字，5 分钟过期
- 5 次错误尝试后锁定 15 分钟
- 绑定文件权限 `0600`

### 访问模式

| 模式 | 说明 | 认证方式 |
|------|------|---------|
| **Public** | 完全公开，任何人访问 | 无需认证 |
| **Protected** | 团队/内部，同事或其他 agent 可访问 | JWT token 或配对码 |
| **Private** | 私人设备，严格管控 | 配对码绑定 |

**Protected vs Private**：两者权限控制相同（未认证只能安全方法），区别是使用场景。Protected 面向团队协作，以 JWT 认证为主；Private 面向个人使用，以配对码绑定为主。两种模式都支持另一种认证方式作为备用。

### 两级认证

**绑定（Bind）** — 独占，全权限
- `aginx pair` 生成配对码
- 客户端 `bindDevice(pairCode, deviceName)` → 返回 token
- 只能绑定一个设备（主人身份）
- 拥有所有权限，包括系统方法

**授权（Auth）** — 多客户端，受限权限
- `aginx auth -n "同事的Mac" -a claude,translator -m listAgents,prompt` → 生成 JWT token
- 客户端 `initialize` 直接带 JWT token → 认证通过
- 可授权多个客户端，每个独立权限
- 权限通过 JWT claims 控制：
  - `agents`: 允许访问的 agent 列表（空 = 全部）
  - `methods`: 允许调用的方法列表（空 = 全部）
  - `sys`: 是否允许系统方法（listDirectory, readFile 等）
  - `exp`: 过期时间

```bash
# 生成授权 token
aginx auth -n "Team CI" -a deploy,test --system -e 30

# 查看已授权客户端
aginx auths

# 撤销授权
aginx revoke client-xxx
```

### Relay 认证

Relay 服务器可配置 shared secret，aginx 和客户端连接时必须携带正确 token（constant-time 比较防时序攻击）。

### E2EE（端到端加密）

Relay 支持 X25519 ECDH + HKDF-SHA256 + ChaCha20-Poly1305，防止 Relay 读取消息内容。

## agent:// URL

```
# 直连
agent://hostname[:port]

# 中继
agent://{id}.relay.aginx.net
```

## License

MIT
