# Aginx - Agent Protocol 实现

> 让访问 Agent 像访问网站一样简单

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
| `claude` | Claude CLI | id, name, capabilities |
| `process` | 外部进程 | id, name, capabilities, command |

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

**sendMessage**
```json
{"jsonrpc": "2.0", "id": "3", "method": "sendMessage", "params": {
  "agentId": "claude",
  "message": "你好"
}}
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
