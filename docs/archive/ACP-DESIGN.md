# Aginx ACP 协议实现设计

> 让 aginx 成为标准的 ACP Agent，支持任何 ACP 兼容客户端连接

## 概述

### 目标

让 aginx 实现ACP (Agent Client Protocol) 协议的 **Agent 侧**，这样：

- IDE (Zed, Cursor, VS Code) 可以直接通过 ACP 协议连接 aginx
- OpenClaw 可以用 aginx 作为 CLI 后端，不需要自己管理进程
- 保持 aginx 现有的 Relay、会话管理等功能不变

### 协议角色

```
┌─────────────────┐                    ┌─────────────────┐
│   ACP Client    │                    │   ACP Agent     │
│   (IDE/OpenClaw)│◄─── ACP 协议 ─────►│    (aginx)      │
│                 │   ndjson/stdin-out │                 │
└─────────────────┘                    └────────┬────────┘
                                                │
                                                ▼
                                        ┌─────────────────┐
                                        │  Internal Agent │
                                        │  (Claude CLI)   │
                                        └─────────────────┘
```

**aginx 作为 ACP Agent**，接收 ACP 请求，转发给内部管理的 Agent (Claude CLI 等)。

---

## 架构设计

### 整体架构

```
                         ┌─────────────────────────────────────────┐
                         │              aginx (Rust)               │
                         │                                         │
    ACP Client           │  ┌─────────────────────────────────┐   │
    ┌──────────┐         │  │        ACP Handler              │   │
    │   Zed    │         │  │  - ndjson 解析                  │   │
    │  Cursor  │──ACP──►│  │  - 方法路由                      │   │
    │OpenClaw │         │  │  - 事件转换                      │   │
    └──────────┘         │  └──────────────┬──────────────────┘   │
                         │                 │                       │
                         │  ┌──────────────▼──────────────────┐   │
                         │  │      Session Manager (现有)      │   │
                         │  │  - 会话创建/管理                  │   │
                         │  │  - Agent 进程管理                 │   │
                         │  └──────────────┬──────────────────┘   │
                         │                 │                       │
                         │  ┌──────────────▼──────────────────┐   │
                         │  │      Agent Runner               │   │
                         │  │  - Claude CLI                   │   │
                         │  │  - Codex CLI                    │   │
                         │  │  - 自定义 CLI                    │   │
                         │  └─────────────────────────────────┘   │
                         │                                         │
                         └─────────────────────────────────────────┘
```

### 启动模式

aginx 支持两种 ACP 模式：

```bash
# 模式1: stdio 模式 (被 IDE/其他程序启动)
aginx acp --stdio

# 模式2: 网络模式 (作为独立服务，接收 ACP over WebSocket)
aginx acp --port 86 --agent claude
```

---

## ACP 协议实现

### 消息格式

ACP 使用 ndjson (newline-delimited JSON) 格式：

```json
{"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {...}}
{"jsonrpc": "2.0", "id": 1, "result": {...}}
{"jsonrpc": "2.0", "method": "sessionUpdate", "params": {...}}
```

### 核心方法

| 方法 | 说明 | aginx 实现 |
|------|------|-----------|
| `initialize` | 握手，交换能力 | 返回 aginx 能力信息 |
| `newSession` | 创建新会话 | 调用现有 SessionManager |
| `loadSession` | 加载已有会话 | 恢复已有会话 |
| `prompt` | 发送消息 | 调用 Agent，流式返回 |
| `cancel` | 取消当前操作 | 中止 Agent 执行 |
| `listSessions` | 列出会话 | 返回会话列表 |

### 核心回调 (aginx 发送给 Client)

| 方法 | 说明 | 触发时机 |
|------|------|---------|
| `sessionUpdate` | 会话状态更新 | Agent 输出、工具调用等 |
| `requestPermission` | 权限请求 | Agent 需要授权时 |

---

## 详细设计

### 1. initialize

**请求:**
```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "initialize",
  "params": {
    "protocolVersion": "0.15.0",
    "clientCapabilities": {
      "fs": { "readTextFile": true, "writeTextFile": true },
      "terminal": true
    },
    "clientInfo": {
      "name": "zed",
      "version": "1.0.0"
    }
  }
}
```

**响应:**
```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "protocolVersion": "0.15.0",
    "agentCapabilities": {
      "loadSession": true,
      "promptCapabilities": {
        "image": true,
        "audio": false,
        "embeddedContext": true
      }
    },
    "agentInfo": {
      "name": "aginx",
      "title": "Aginx ACP Gateway",
      "version": "0.1.0"
    },
    "authMethods": []
  }
}
```

### 2. newSession

**请求:**
```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "newSession",
  "params": {
    "cwd": "/Users/user/projects/myproject",
    "mcpServers": [],
    "_meta": {
      "agentId": "claude"
    }
  }
}
```

**响应:**
```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "result": {
    "sessionId": "sess_abc123"
  }
}
```

**实现逻辑:**
1. 从 `_meta.agentId` 获取要使用的 Agent (默认 "claude")
2. 调用现有 `SessionManager::create_session(agent_id, cwd)`
3. 返回 sessionId

### 3. prompt

**请求:**
```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "prompt",
  "params": {
    "sessionId": "sess_abc123",
    "prompt": [
      { "type": "text", "text": "写一个 hello world" }
    ]
  }
}
```

**响应 (流式事件):**
```json
{"jsonrpc": "2.0", "method": "sessionUpdate", "params": {"sessionId": "sess_abc123", "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "好的，我来写..."}}}}
{"jsonrpc": "2.0", "method": "sessionUpdate", "params": {"sessionId": "sess_abc123", "update": {"sessionUpdate": "tool_call", "toolCallId": "tc_1", "title": "write: hello.py", "status": "in_progress"}}}
{"jsonrpc": "2.0", "method": "sessionUpdate", "params": {"sessionId": "sess_abc123", "update": {"sessionUpdate": "tool_call_update", "toolCallId": "tc_1", "status": "completed"}}}}
{"jsonrpc": "2.0", "id": 3, "result": {"stopReason": "end_turn"}}
```

**实现逻辑:**
1. 提取 prompt 中的文本内容
2. 调用 Agent (Claude CLI)，使用 `--output-format stream-json`
3. 解析 stream-json 输出，转换为 ACP `sessionUpdate` 事件
4. 遇到权限请求时，发送 `requestPermission`

### 4. requestPermission (权限转发)

当 Agent 需要权限时，aginx 转发给 Client:

**aginx → Client:**
```json
{
  "jsonrpc": "2.0",
  "method": "requestPermission",
  "params": {
    "requestId": "req_123",
    "toolCall": {
      "toolCallId": "tc_1",
      "title": "write: /path/to/file.py",
      "_meta": {
        "toolName": "write"
      }
    },
    "description": "Allow writing to /path/to/file.py?",
    "options": [
      { "optionId": "1", "label": "Yes, allow once", "kind": "allow_once" },
      { "optionId": "2", "label": "Yes, always allow", "kind": "allow_always" },
      { "optionId": "3", "label": "No, reject", "kind": "reject_once" }
    ]
  }
}
```

**Client → aginx (响应):**
```json
{
  "jsonrpc": "2.0",
  "id": "req_123",
  "result": {
    "outcome": { "outcome": "selected", "optionId": "1" }
  }
}
```

---

## 与现有 aginx 集成

### 会话管理复用

```
ACP newSession ──────► SessionManager::create_session()
                              │
                              ├── 创建 Agent 进程
                              ├── 分配 sessionId
                              └── 返回 sessionId

ACP prompt ──────────► SessionManager::get_session(sessionId)
                              │
                              ├── 获取 Agent 进程的 stdin/stdout
                              ├── 发送消息
                              └── 流式读取响应
```

### 权限处理流程

```
                        ┌─────────────────────────────────────┐
                        │            aginx                    │
                        │                                     │
Claude CLI 输出          │  ┌───────────────────────────┐    │
权限请求文本             │  │ stream-json 解析器        │    │
───────┬────────────────►│  │ 检测到 permission_denial  │    │
       │                 │  └───────────┬───────────────┘    │
       │                 │              │                     │
       │                 │  ┌───────────▼───────────────┐    │
       │                 │  │ 构造 requestPermission    │    │
       │                 │  │ 发送给 ACP Client         │    │
       │                 │  └───────────┬───────────────┘    │
       │                 │              │                     │
       │                 │  ┌───────────▼───────────────┐    │
       │                 │  │ 等待 Client 响应          │    │
       │                 │  └───────────┬───────────────┘    │
       │                 │              │                     │
       │                 │  ┌───────────▼───────────────┐    │
       │                 │  │ 将选择转换为 CLI 输入     │    │
       │◄────────────────│  │ 发送给 Claude CLI         │    │
       │                 │  └───────────────────────────┘    │
       │                 │                                     │
                        └─────────────────────────────────────┘
```

---

## 配置扩展

### aginx.toml 新增配置

```toml
[server]
# ... 现有配置 ...

[acp]
# 启用 ACP 协议支持
enabled = true
# ACP 协议版本
protocol_version = "0.15.0"
# 默认 Agent
default_agent = "claude"

# 权限策略
[acp.permissions]
# 自动批准的安全工具
auto_approve = ["read", "search", "web_search", "memory_search"]
# 需要确认的危险工具
require_confirm = ["write", "edit", "exec", "delete"]
```

---

## 与 App 集成

当 aginx 作为 Relay 模式时，权限请求可以转发给 App:

```
┌─────────┐      ACP       ┌─────────┐    WebSocket    ┌─────────┐
│   Zed   │◄──────────────►│  aginx  │◄───────────────►│   App   │
│  (IDE)  │                │ (Relay) │                 │(Android)│
└─────────┘                └─────────┘                 └─────────┘
                                │
                                │ requestPermission
                                │ 转发给 App
                                ▼
                           App 显示对话框
                           用户选择
                                │
                                ▼
                           返回选择给 aginx
                           aginx 转发给 Agent
```

---

## 实现优先级

### Phase 1: 基础 ACP 支持
- [ ] ndjson 解析/序列化
- [ ] `initialize` 方法
- [ ] `newSession` / `loadSession` 方法
- [ ] `prompt` 方法 (基础流式输出)

### Phase 2: 工具调用支持
- [ ] 解析 stream-json 中的工具调用
- [ ] 转换为 ACP `tool_call` / `tool_call_update` 事件
- [ ] 文件位置提取

### Phase 3: 权限处理
- [ ] 检测权限请求
- [ ] `requestPermission` 回调
- [ ] 权限响应处理
- [ ] 转发给 App (Relay 模式)

### Phase 4: 完善功能
- [ ] `listSessions` 方法
- [ ] `cancel` 方法
- [ ] 错误处理
- [ ] 会话恢复

---

## 参考

- [ACP SDK](https://www.npmjs.com/package/@agentclientprotocol/sdk)
- [OpenClaw ACP Bridge](https://github.com/openclaw/openclaw/blob/main/docs.acp.md)
- [Claude CLI stream-json format](https://docs.anthropic.com/en/docs/claude-code)
