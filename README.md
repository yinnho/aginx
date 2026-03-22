# Aginx - Agent Protocol 实现

> 让访问 Agent 像访问网站一样简单

## 基本原则

### 1. Aginx 是纯粹的消息路由器

Aginx **不关心** Agent 内部如何实现：
- 用云端 API 还是本地服务
- 用什么语言、什么框架
- 如何处理请求

Aginx **只负责**：
```
接收请求 --> 路由到 Agent --> 返回响应
```

### 2. Agent 内部实现与 Aginx 无关

```
Aginx --> Claude Agent --> 调用云端 API（Agent 内部）
Aginx --> OpenCarrier Agent --> 调用本地服务（Agent 内部）
Aginx --> Weather Agent --> 调用第三方 API（Agent 内部）
```

Agent 用什么、怎么实现，是 Agent 自己的事情。

### 3. 任何 CLI 都可以是 Agent

只要能通过命令行调用并返回结果，就能包装成 Agent。

---

## 核心理念

一个 URL 搞定所有连接：

```
agent://rcs0aj94.relay.yinnho.cn
```

客户端不需要知道：
- 服务器 IP 地址
- 端口号
- 协议细节
- Agent ID

只需要这一个 URL，就像访问 `https://google.com` 一样简单。

---

## 架构

```
┌─────────────┐                    ┌─────────────┐                    ┌─────────────┐
│   客户端     │─── agent://URL ───▶│    Relay    │──── TCP/JSON-RPC ──▶│   Aginx     │
│             │                    │  (中继服务器) │                    │  (Agent宿主) │
└─────────────┘                    └─────────────┘                    └─────────────┘
                                                                              │
                                                                              ▼
                                                                       ┌─────────────┐
                                                                       │   Agents    │
                                                                       │ - Claude    │
                                                                       │ - Echo      │
                                                                       │ - Process   │
                                                                       └─────────────┘
```

### 两种模式

| 模式 | 说明 | 使用场景 |
|------|------|----------|
| **Relay** | 通过中继服务器 | 内网环境、NAT 穿透 |
| **Direct** | 直接 TCP 监听 | 公网服务器、有公网 IP |

---

## 快速开始

### 安装

```bash
cargo install --path .
```

### 启动

```bash
# 默认 relay 模式，自动申请 ID
aginx

# 直连模式
aginx --mode direct
```

### 启动输出

```
========================================
aginx v0.1.0
运行模式: Relay
访问地址: agent://rcs0aj94.relay.yinnho.cn
========================================
```

把这个 URL 分享给客户端即可连接。

---

## 配置

配置文件：`~/.aginx/config.toml`

### Agent 配置（类似 nginx server blocks）

```toml
# 内置 Agent
[[agents.list]]
id = "echo"
name = "Echo Agent"
agent_type = "builtin"
capabilities = ["echo"]

# Claude Agent（调用本地 claude CLI）
[[agents.list]]
id = "claude"
name = "Claude Agent"
agent_type = "claude"
capabilities = ["chat", "code", "ask"]

# 进程 Agent（调用外部程序）
[[agents.list]]
id = "weather"
name = "Weather Agent"
agent_type = "process"
capabilities = ["weather", "forecast"]
command = "/usr/local/bin/weather-agent"
args = ["--verbose"]
working_dir = "/tmp"
[agents.list.env]
API_KEY = "your-api-key"
```

### Agent 类型

| 类型 | 说明 | 必填字段 |
|------|------|----------|
| `builtin` | 内置 agent | id, name, capabilities |
| `claude` | Claude CLI | id, name, capabilities, help_command (可选) |
| `process` | 外部进程 | id, name, capabilities, command, help_command (可选) |

### help_command 字段

用于指定获取 agent 帮助信息的命令：

```toml
[[agents.list]]
id = "claude"
name = "Claude Agent"
agent_type = "claude"
capabilities = ["chat", "code", "ask"]
help_command = "claude --help"

[[agents.list]]
id = "weather"
name = "Weather Agent"
agent_type = "process"
command = "/usr/local/bin/weather-agent"
help_command = "/usr/local/bin/weather-agent --help"
```

如果未配置 `help_command`，会自动尝试 `{command} --help`

---

## 客户端使用

### 连接

```bash
# 测试连接
cargo run --example test_client_relay agent://rcs0aj94.relay.yinnho.cn

# 调用 Claude Agent
cargo run --example test_claude_agent agent://rcs0aj94.relay.yinnho.cn "你好"
```

### JSON-RPC API

**getServerInfo**
```json
{"jsonrpc": "2.0", "id": "1", "method": "getServerInfo", "params": {}}
```

**listAgents**
```json
{"jsonrpc": "2.0", "id": "2", "method": "listAgents", "params": {}}
```

**getAgentHelp**
```json
{"jsonrpc": "2.0", "id": "3", "method": "getAgentHelp", "params": {
  "agentId": "claude"
}}
```

**Response:**
```json
{
  "agentId": "claude",
  "help": "claude [options] [prompt]\n  --print    非交互模式，  ..."
}
```

**sendMessage**
```json
{"jsonrpc": "2.0", "id": "4", "method": "sendMessage", "params": {
  "agentId": "claude",
  "message": "你好"
}
```

---

## 会话管理

### 概念

**会话 (Session)** = 一个长期运行的 agent 进程

同一个会话内的多次消息在**同一个进程**里处理，保持上下文：

```
客户端 A 创建会话
  └── 启动 claude 进程 A
  └── 消息1 "你是谁？"     --> 同一个进程
  └── 消息2 "写个hello"   --> 同一个进程（有上下文）
  └── 消息3 "再加个注释"   --> 同一个进程（知道"hello"是什么）
```

### 会话 API

**createSession** - 创建会话
```json
{"jsonrpc": "2.0", "id": "1", "method": "createSession", "params": {
  "agentId": "claude"
}}
```

**Response:**
```json
{
  "result": {
    "sessionId": "sess_abc123",
    "agentId": "claude"
  }
}
```

**sendMessage** - 发送消息（使用会话）
```json
{"jsonrpc": "2.0", "id": "2", "method": "sendMessage", "params": {
  "sessionId": "sess_abc123",
  "message": "你好"
}}
```

**closeSession** - 关闭会话
```json
{"jsonrpc": "2.0", "id": "3", "method": "closeSession", "params": {
  "sessionId": "sess_abc123"
}}
```

### 并发控制

```toml
[server]
max_concurrent_sessions = 10    # 最大并发会话数
session_timeout_seconds = 1800  # 会话超时（30分钟）
```

- 达到上限时，`createSession` 返回错误或排队等待
- 超时未活动的会话自动关闭

### 会话流程

```
┌─────────────────────────────────────────────────────────────┐
│                        客户端                                │
└─────────────────────────────────────────────────────────────┘
          │
          │ 1. createSession("claude")
          ▼
┌─────────────────────────────────────────────────────────────┐
│  SessionManager                                              │
│  ┌─────────────────┐                                        │
│  │ 检查并发限制     │ ← semaphore (max=10)                   │
│  └────────┬────────┘                                        │
│           │                                                  │
│           ▼                                                  │
│  ┌─────────────────┐                                        │
│  │ 启动 claude 进程 │                                        │
│  │ stdin/stdout    │                                        │
│  └────────┬────────┘                                        │
│           │                                                  │
│           ▼                                                  │
│  ┌─────────────────┐                                        │
│  │ 返回 sessionId  │                                        │
│  │ "sess_abc123"   │                                        │
│  └─────────────────┘                                        │
└─────────────────────────────────────────────────────────────┘
          │
          │ 2. sendMessage("sess_abc123", "你好")
          ▼
┌─────────────────────────────────────────────────────────────┐
│  找到会话 → 写入 stdin → 读取 stdout → 返回响应             │
│  （进程保持运行，有上下文）                                   │
└─────────────────────────────────────────────────────────────┘
          │
          │ 3. closeSession("sess_abc123")
          ▼
┌─────────────────────────────────────────────────────────────┐
│  关闭进程 → 释放 semaphore                                   │
└─────────────────────────────────────────────────────────────┘
```

---

## 项目结构

```
aginx/
├── src/
│   ├── main.rs           # 入口
│   ├── config/           # 配置管理
│   ├── agent/            # Agent 管理
│   ├── relay/            # Relay 客户端
│   ├── server/           # 直连服务器
│   └── protocol/         # JSON-RPC 协议
├── examples/
│   ├── test_client_relay.rs    # 测试客户端
│   └── test_claude_agent.rs    # Claude Agent 测试
└── aginx-relay/          # Relay 服务器（独立项目）
```

---

## 待改进功能

### 高优先级

- [ ] **认证机制** - API Key / JWT 认证
- [ ] **WebSocket 支持** - 浏览器直连
- [ ] **Agent 热加载** - 不重启更新配置
- [ ] **日志持久化** - 请求/响应日志
- [x] **会话管理** - 长连接会话、上下文保持、并发控制

### 中优先级

- [ ] **连接池** - 复用 TCP 连接
- [ ] **流量控制** - 限流、配额
- [ ] **监控指标** - Prometheus metrics
- [ ] **配置校验** - 启动时校验配置

### 低优先级

- [ ] **集群支持** - 多 aginx 实例
- [ ] **Agent 市场** - 发现和分享 agent
- [ ] **Web UI** - 管理界面

---

## 相关项目

- [aginx-relay](https://github.com/yinnho/aginx-relay) - Relay 服务器

---

## License

MIT
