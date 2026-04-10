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
| aginx-app | Chrome | 用户产品（App） |
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
└── aginx-app/        # 用户 App (Android) - 已废弃，后续从 aginxium 重建
```

## 快速开始

### 安装

**一键安装（推荐）**

```bash
curl -fsSL https://raw.githubusercontent.com/yinnho/aginx/main/install.sh | sh
```

**从 GitHub Release 下载**

到 [Releases](https://github.com/yinnho/aginx/releases) 页面下载对应平台的二进制。

**从源码编译**

需要 Rust 工具链：

```bash
cargo install --path .
# 或
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
# 通过 ACP 方法注册：
# discoverAgents → 扫描 → registerAgent → 注册
```

## 协议

Aginx 使用 **ACP (Agent Client Protocol)**，基于 JSON-RPC 2.0 over TCP (ndjson)。

### 核心 ACP 方法

| 方法 | 说明 |
|------|------|
| `initialize` | 握手，交换能力 |
| `session/new` | 创建会话 |
| `session/load` | 恢复已有会话 |
| `session/prompt` | 发送消息（流式） |
| `session/cancel` | 取消 |

### 流式响应

`session/prompt` 返回 `{"streaming": true}` 后，通过 `sessionUpdate` 通知推送：

```json
{"method":"sessionUpdate","params":{"sessionId":"xxx","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"..."}}}}
{"method":"sessionUpdate","params":{"sessionId":"xxx","update":{"sessionUpdate":"tool_call","toolCallId":"tc_1","title":"Read(file.rs)","status":"in_progress"}}}
{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn","response":"..."}}
```

### Aginx 扩展方法

| 方法 | 说明 |
|------|------|
| `listAgents` | 列出已注册 Agent |
| `discoverAgents` | 扫描发现新 Agent |
| `registerAgent` | 注册 Agent |
| `listConversations` | 列出对话 |
| `getMessages` | 获取消息历史 |
| `deleteConversation` | 删除对话 |
| `listSessions` | 列出会话 |
| `getMessages` | 获取消息历史 |
| `deleteConversation` | 删除对话 |
| `listDirectory` | 浏览服务器目录 |
| `readFile` | 读取服务器文件 |
| `bindDevice` | 绑定设备 |

## agent:// URL

```
# 直连
agent://hostname[:port]

# 中继
agent://{id}.relay.aginx.net
```

## 部署

```bash
# 同步代码
rsync -avz --exclude 'target' --exclude '.git' -e ssh ./ 86quan:/data/www/aginx/

# 编译
ssh 86quan "cd /data/www/aginx && cargo build --release"

# 重启
ssh 86quan "pkill -f aginx ; nohup /data/www/aginx/target/release/aginx > /data/www/aginx/aginx.log 2>&1 &"
```

## License

MIT
