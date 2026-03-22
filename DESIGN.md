# aginx 设计文档

## 项目概述

aginx 是一个统一的 Agent Protocol 实现，让用户可以像访问网站一样访问 Agent。

### 核心理念

```
https://baidu.com     →     agent://baidu.com
```

- 输入 URL 即可访问
- 支持直连和中继两种模式
- 内网自动穿透，外网随时访问
- 默认公开访问

## 项目信息

| 项目 | 值 |
|------|-----|
| 项目名 | **aginx** |
| 域名 | yinnho.cn |
| 中继地址 | `agent://xxx.relay.yinnho.cn` |
| Relay 地址 | `relay.yinnho.cn:8600` (aginx 连接 relay 使用) |

---

## 核心协议

### agent:// = TCP 86 + JSON-RPC 2.0

**无论直连还是 relay，App 都是用 TCP 86 端口 + JSON-RPC 2.0 协议通信。**

### 关键原则

1. **App 不需要 SDK** - 直接用 TCP + JSON-RPC
2. **relay 只是中转站** - 对 App 来说就是普通的 TCP 服务
3. **aginx 只有一种服务模式** - TCP 86 端口，JSON-RPC 2.0 协议
4. **relay 模式下**:
   - aginx 在内网，主动连接 relay (**纯 TCP 长连接**，不是 WebSocket)
   - relay 保存 aginx 的连接
   - 外网 App 通过 TCP 连接 relay
   - relay 通过已建立的 TCP 连接转发给 aginx

### 为什么不用 WebSocket

| 考虑 | 纯 TCP | WebSocket |
|------|--------|-----------|
| 端口依赖 | 任意端口 | 通常需要 80/443 |
| 协议复杂度 | 简单 | 有握手/帧格式 |
| 通用性 | 高 | 需要特定环境 |
| 心跳 | 自定义 | 内置 |

**结论**: relay ↔ aginx 使用纯 TCP 长连接，更简单、更通用。

---

## 架构

### 整体架构图

```
┌─────────────────────────────────────────────────────────────────┐
│                          App                                    │
│                                                                 │
│   输入: agent://baidu.com 或 agent://xxx.relay.yinnho.cn       │
│                                                                 │
│   解析 URL:                                                     │
│   - agent://baidu.com → TCP 连接到 baidu.com:86               │
│   - agent://xxx.relay.yinnho.cn → TCP 连接到 relay.yinnho.cn:86│
│                                                                 │
│   协议: TCP 86 端口 + JSON-RPC 2.0 (纯协议，无额外层)           │
└─────────────────────────────────────────────────────────────────┘
                              │
                              ▼
              ┌───────────────────┴───────────────────┐
              │                                       │
              ▼                                       ▼
┌──────────────────────┐        ┌──────────────────────────────────┐
│       直连           │        │            Relay                 │
│                      │        │                                  │
│  baidu.com:86        │        │  对外: TCP 86 (接收App)          │
│      │               │        │  对内: TCP 8600 (aginx 连接)     │
│      ▼               │        │        │                         │
│  ┌────────────┐      │        │        │ 查表找 xxx 对应的 aginx │
│  │  aginx     │      │        │        │         │               │
│  │  TCP:86    │      │        │        │         ▼               │
│  └────────────┘      │        │  ┌─────────────────────┐       │
│                      │        │  │ aginx (内网)          │       │
└──────────────────────┘        │  │   TCP → relay:8600   │       │
                                │  └─────────────────────┘       │
                                └──────────────────────────────────┘
```

### 数据流

#### 直连模式
```
App ──TCP 86 + JSON-RPC──→ baidu.com:86 (aginx)
```

#### 中继模式
```
App ──TCP 86 + JSON-RPC──→ relay:86──TCP 长连接──→ aginx (内网)
```

### 各组件职责

| 组件 | 职责 |
|------|------|
| **App** | 解析 agent:// URL，TCP 连接，发送 JSON-RPC |
| **aginx** | TCP 86 服务(本地)，TCP 长连接到 relay(中继模式) |
| **Relay** | TCP 86 服务(对外) + TCP 8600 服务(对内)，中转消息 |

**关键点**:
- App 只需要实现 TCP + JSON-RPC，不需要 SDK
- relay 对 App 来说就是普通的 TCP 服务
- aginx 无论直连还是 relay 模式，本地都是 TCP 86 服务
- relay ↔ aginx 使用纯 TCP 长连接，不依赖 WebSocket

## agent:// URL 格式

### 直连模式

```
agent://hostname[:port][/path]
agent://baidu.com
agent://192.168.1.100:86
agent://myserver.local/hotel
```

### 中继模式

```
agent://{id}.relay.yinnho.cn[/path]
agent://abc123.relay.yinnho.cn
agent://abc123.relay.yinnho.cn/hotel
```

## aginx 安装和使用流程

### 1. 安装

```bash
# 方式一: Cargo 安装
cargo install aginx

# 方式二: 从源码编译
git clone https://github.com/yinnho/aginx.git
cd aginx
cargo install --path .
```

### 2. 配置

```bash
# 初始化配置
aginx init

# 编辑配置文件
vim ~/.aginx/config.toml
```

### 3. 启动

```bash
aginx
```

根据配置文件决定访问方式：
- `relay.enabled = true` → 连接中继，获得 `agent://xxx.relay.yinnho.cn`
- `relay.enabled = false` → 使用直连地址

## 配置文件

### aginx 两种模式

| 模式 | 说明 | 配置 |
|------|------|------|
| **direct** | 直连模式，监听本地 TCP 86 端口 | `mode = "direct"` |
| **relay** | 中继模式，连接 relay 服务器 | `mode = "relay"` |

### 配置示例

```toml
# ~/.aginx/config.toml

[server]
# 运行模式: direct(直连) | relay(中继)
mode = "relay"

# 本地服务配置 (两种模式都需要，本地调用)
port = 86
host = "0.0.0.0"
# 访问模式: public (公开) | private (私有)
access = "public"

# 中继模式配置 (mode = "relay" 时使用)
[relay]
# 完整地址，包含 aginx ID
# 格式: {aginx_id}.relay.yinnho.cn:8600
url = "abc123.relay.yinnho.cn:8600"

# 直连模式配置 (mode = "direct" 时使用)
# [direct]
# public_url = "agent://myserver.com"

# 私有模式认证配置
# [auth]
# jwt_secret = "your-secret-key"

[agents]
# 内置 agents
[[agents.builtin]]
id = "echo"
name = "Echo Agent"
description = "回显消息"

# 进程 agents
[[agents.process]]
id = "hotel"
name = "酒店服务"
command = "/path/to/hotel-agent"
args = ["--mode", "cli"]
```

### 场景配置

#### 场景 1: 内网，使用中继 (默认)

```toml
[server]
mode = "relay"
access = "public"

[relay]
# 完整地址，aginx ID 在 URL 中
url = "abc123.relay.yinnho.cn:8600"

# 启动后:
# - aginx 连接 abc123.relay.yinnho.cn:8600
# - 外部访问: agent://abc123.relay.yinnho.cn
```

#### 场景 2: 有公网域名，直连

```toml
[server]
mode = "direct"
access = "public"

[direct]
public_url = "agent://myserver.com"

# 启动后:
# - aginx 监听本地 TCP 86
# - 外部访问: agent://myserver.com
```

#### 场景 3: 私有服务，需要认证

```toml
[server]
mode = "relay"
access = "private"

[relay]
url = "myprivate.relay.yinnho.cn:8600"

[auth]
jwt_secret = "your-secret-key"

# 启动后:
# - aginx 连接 relay
# - 外部访问: agent://myprivate.relay.yinnho.cn (需要认证)
```

## 访问权限

### 公开模式 (默认)

- 任何人都可以访问
- 无需认证
- 适用于公共服务
- 易于推广使用

### 私有模式 (可选)

- 需要认证才能访问
- 支持 JWT 认证
- 适用于公司内部、个人私密工具

## Relay Server

### 定位

- **只开放给 aginx 使用**
- 不是一个通用的 WebSocket 中继服务
- 只支持 Agent Protocol

### 功能

1. **用户注册** - 注册即成为 aginx 社区用户
2. **中继通道管理** - 为每个 aginx 分配唯一 ID
3. **连接保持** - aginx 主动连接并保持心跳
4. **消息转发** - 转发用户 ↔ aginx 的消息

### 商业模式

| 阶段 | 策略 |
|------|------|
| 初期 | 免费，不限量 |
| 后期 | 开套餐，基础免费 + 付费升级 |

## agent:// 协议实现

### 核心原则

**agent:// = TCP 86 + JSON-RPC 2.0**

无论直连还是 relay，App 都是用 TCP 86 端口 + JSON-RPC 2.0 协议通信。

### App 端实现

App **不需要 SDK**，只需要：

1. 解析 agent:// URL
2. TCP 连接到目标主机:86
3. 发送 JSON-RPC 2.0 消息
4. 接收 JSON-RPC 2.0 响应

```kotlin
// App 端伪代码
class AgentClient(private val url: String) {
    fun send(method: String, params: Map<String, Any>): String {
        val host = parseHost(url)  // baidu.com 或 relay.yinnho.cn
        val socket = Socket(host,86)

        // 发送 JSON-RPC
        socket.write(jsonRpcRequest(method, params))

        // 接收响应
        return socket.readLine()
    }
}
```

### agent:// URL 解析

```
agent://abc123.relay.yinnho.cn
        │       │      │
        │       │      └── 域名 (relay.yinnho.cn)
        │       └── 固定的中继标识
        └── aginx ID

App 解析后:
- TCP 连接: relay.yinnho.cn:86
- relay 内部路由到: abc123
```

```
agent://baidu.com
        │
        └── 域名

App 解析后:
- TCP 连接: baidu.com:86
```

### Relay 内部消息路由

Relay 对 App 透明，内部需要路由消息到正确的 aginx：

```
App → Relay:          {"jsonrpc":"2.0","id":1,"method":"getServerInfo"}
                         │
                         ▼
Relay → Aginx:         {"type":"data","client_id":"c_xxx","data":{...}}
                         │
                         ▼
Aginx 处理并响应
                         │
                         ▼
Aginx → Relay:         {"clientId":"c_xxx","jsonrpc":"2.0","result":{...}}
                         │
                         ▼
Relay → App:           {"jsonrpc":"2.0","result":{...}}
```

**注意**: 消息包装只在 Relay ↔ Aginx 之间，App 看到的永远是纯 JSON-RPC。

### 关键设计点

1. **App 视角**: 只看到纯 JSON-RPC 2.0，TCP 连接
2. **Relay 视角**:
   - 对外: TCP 86 服务 (接收 App)
   - 对内: TCP 8600 服务 (aginx 连接)
   - 路由: 根据 URL 中的 aginx ID 转发
3. **Aginx 视角**:
   - 本地: TCP 86 服务
   - 中继模式: TCP 长连接 relay:8600
   - 处理: JSON-RPC 请求，返回响应

## 消息协议

基于 JSON-RPC 2.0。

### 请求示例

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "sendMessage",
  "params": {
    "agentId": "hotel",
    "message": "查询北京酒店"
  }
}
```

### 响应示例

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "response": "找到 10 家北京酒店..."
  }
}
```

## 心跳机制

### 参数

| 参数 | 值 | 说明 |
|------|-----|------|
| Ping 间隔 | 30秒 | Aginx 每隔30秒发送心跳 |
| 超时时间 | 90秒 | 超过90秒无响应则断开 |

### 实现方式

使用应用层心跳消息 (纯 TCP):

```json
{"type":"ping"}
```

```
Aginx ──{"type":"ping"}──> Relay
Relay ──{"type":"pong"}──> Aginx
```

- 简单的应用层协议
- 不依赖 WebSocket
- 3次 Ping 无 Pong 则断开连接 (90秒)

## DNS 配置

```
# yinnho.cn

# 固定子域名
relay.yinnho.cn    A      Relay服务器IP

# 中继泛解析
*.relay.yinnho.cn  CNAME  relay.yinnho.cn
```

## 项目结构

```
aginx/
├── Cargo.toml
├── src/
│   ├── main.rs              # 入口
│   ├── config.rs            # 配置
│   ├── server/              # 本地服务
│   │   ├── mod.rs
│   │   ├── tcp.rs           # TCP 服务
│   │   └── handler.rs       # 请求处理
│   ├── relay/               # 中继客户端
│   │   ├── mod.rs
│   │   └── client.rs        # TCP 长连接客户端
│   ├── agent/               # Agent 管理
│   │   ├── mod.rs
│   │   ├── manager.rs       # Agent 管理器
│   │   └── process.rs       # 进程调用
│   ├── auth/                # 认证
│   │   ├── mod.rs
│   │   └── jwt.rs           # JWT 认证
│   └── protocol/            # 协议
│       └── mod.rs
├── tests/
└── docs/
    └── DESIGN.md            # 本文档
```

## 已确认决策

| 项目 | 决定 |
|------|------|
| 项目名 | **aginx** |
| 域名 | yinnho.cn |
| URL 格式 | `agent://xxx.relay.yinnho.cn` |
| Relay 内部地址 | `relay.yinnho.cn:8600` (aginx 连接) |
| Relay 对外地址 | `relay.yinnho.cn:86` (App 连接) |
| Relay ↔ Aginx | **纯 TCP 长连接**，不用 WebSocket |
| 启动流程 | 根据配置文件决定直连/中继 |
| aginx 访问权限 | 默认公开 |
| Relay 服务 | 只开放给 aginx 使用 |
| 用户模式 | 注册使用，初期免费不限量，后期套餐 |

## Agent CLI 规范

使用 JSON-RPC 2.0 标准协议。

### CLI 输入 (stdin)

```json
{"jsonrpc": "2.0", "id": 1, "method": "query", "params": {"city": "北京"}}
```

### CLI 输出 (stdout)

```json
{"jsonrpc": "2.0", "id": 1, "result": {"message": "找到10家酒店..."}}
```

### CLI 错误输出 (stdout)

```json
{"jsonrpc": "2.0", "id": 1, "error": {"code": -32600, "message": "Invalid params"}}
```

## 与现有项目关系

### agent-cli (Rust)

- **复用大部分代码** - 复制到 aginx 项目
- 主要复用:
  - `shared/protocol/` - JSON-RPC 协议实现
  - `shared/cert/` - 证书相关
  - `agentd/agent/` - Agent 管理器
  - `agentd/server/` - 服务端框架
- 需要修改以适应 aginx 架构

### yingherelay (TypeScript)

- **用 Rust 重写** - 纳入 aginx 项目
- 功能整合到:
  - `aginx/src/relay/client.rs` - 中继客户端
  - 独立的 Relay Server 项目

## 代码复用计划

```
agent-cli/                        aginx/
├── shared/src/                   ├── src/
│   ├── protocol/      ─────────► │   protocol/    (复制+修改)
│   │   ├── jsonrpc.rs            │
│   │   ├── message.rs            │
│   │   └── parser.rs             │
│   └── cert/         ─────────► │   cert/        (复制+修改)
│                                  │
├── agentd/src/                   │
│   ├── server/       ─────────► │   server/      (复制+修改)
│   └── agent/        ─────────► │   agent/       (复制+修改)
│                                  │
└── (不使用)                      │
                                  ├── relay/       (新增: 中继客户端)
                                  └── auth/        (新增: 认证模块)
```

## 核心流程

```
用户输入 agent:// → 判断在线 → 发消息 → aginx 返回
```

1. 用户绑定设备：输入 `agent://xxx.relay.yinnho.cn`
2. 判断在线：aginx 是否在线
3. 发消息：JSON-RPC 2.0
4. aginx 返回：返回结果

## App 绑定设备

| 旧方式 | 新方式 |
|--------|--------|
| 输入绑定码 | 直接输入 `agent://xxx.relay.yinnho.cn` |

- 绑定 = 访问，一步到位
- 不需要额外的绑定码概念

## 注册流程

```
安装 aginx → 自动生成设备 ID → 配置 → 启动 → 获得 agent:// 地址
```

- 无需邮箱密码
- 设备 ID 就是唯一凭证
- 全部 CLI 完成

## 商业模式

| 阶段 | 策略 |
|------|------|
| 初期 | 免费，不限量 |
| 后期 | 套餐，CLI 输出二维码充值 |

## SDK

- 提供给客户端开发者
- 封装 agent:// 协议 + JSON-RPC 2.0

## 已确认决策

| 项目 | 决定 |
|------|------|
| 项目名 | **aginx** |
| 域名 | yinnho.cn |
| URL 格式 | `agent://xxx.relay.yinnho.cn` |
| Relay 内部地址 | `relay.yinnho.cn:8600` (aginx 连接) |
| Relay 对外地址 | `relay.yinnho.cn:86` (App 连接) |
| Relay ↔ Aginx | **纯 TCP 长连接**，不用 WebSocket |
| 启动流程 | 根据配置文件决定直连/中继 |
| aginx 访问权限 | 默认公开 |
| Relay 服务 | 只开放给 aginx 使用，用户可自建 |
| 注册方式 | CLI 自动生成设备 ID |
| 协议 | **JSON-RPC 2.0 over TCP** |
| 代码复用 | agent-cli (复制+修改)，yingherelay (Rust 重写) |
| 客户端 | 用户自己打造，提供 SDK (可选) |

## 项目结构

```
aginx/                    # 主项目
├── aginx/               # Agent 网关
└── relay-server/        # 中继服务器 (配套项目)
```

## 待讨论

- 还有吗？
