# Aginx Agent ACP Protocol Specification

**版本**: 0.1.0  
**传输**: stdin/stdout, ndjson (每行一个 JSON-RPC 2.0 消息)  
**角色**: aginx 是 client，agent 是 server

## 概述

Agent 接入 aginx 只需实现一个 CLI 程序，在 stdin/stdout 上说 ACP 协议。aginx 启动 agent 进程，通过 stdin 发送请求，从 stdout 读取响应和通知。所有日志输出到 stderr（禁止写 stdout）。

```
aginx ──stdin──→ agent
aginx ←─stdout── agent
```

## 生命周期

```
1. aginx 启动 agent 进程
2. aginx → agent: initialize
3. agent → aginx: InitializeResult（握手完成）

4. 用户发消息时:
   aginx → agent: session/new 或 session/load
   agent → aginx: {sessionId}

   aginx → agent: session/prompt
   agent → aginx: sessionUpdate 通知（流式文本块、工具调用）
   agent → aginx: 最终响应 {stopReason}

5. aginx 关闭 agent 进程（stdin EOF 或进程退出）
```

---

## 方法（aginx → agent）

### initialize

握手。aginx 启动 agent 后第一个调用。

**Request:**
```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "initialize",
  "params": {
    "protocolVersion": "0.1.0",
    "clientInfo": {
      "name": "aginx",
      "version": "0.1.0"
    }
  }
}
```

**Response:**
```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "protocolVersion": "0.1.0",
    "agentInfo": {
      "name": "opencarrier",
      "title": "OpenCarrier",
      "version": "1.0.0"
    },
    "capabilities": {
      "streaming": true,
      "permissions": true
    }
  }
}
```

Agent 必须在收到 `initialize` 后响应。如果协议版本不兼容，返回错误。

---

### session/new

创建新会话。

**Request:**
```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "session/new",
  "params": {
    "agentId": "opencarrier",
    "cwd": "/path/to/working/directory"
  }
}
```

`cwd` 是可选的工作目录。

**Response:**
```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "result": {
    "sessionId": "agent-side-session-uuid"
  }
}
```

`sessionId` 由 agent 生成，用于后续的 prompt 和 load。

---

### session/load

加载已有会话（恢复对话上下文）。

**Request:**
```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "session/load",
  "params": {
    "sessionId": "agent-side-session-uuid",
    "cwd": "/path/to/working/directory"
  }
}
```

**Response:**
```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "result": {
    "sessionId": "agent-side-session-uuid",
    "status": "resumed"
  }
}
```

如果 sessionId 不存在，返回错误：
```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "error": {
    "code": -32603,
    "message": "Session not found: xxx"
  }
}
```

---

### session/prompt

发送用户消息，接收流式响应。

**Request:**
```json
{
  "jsonrpc": "2.0",
  "id": 4,
  "method": "session/prompt",
  "params": {
    "sessionId": "agent-side-session-uuid",
    "prompt": [
      {
        "type": "text",
        "text": "你好，请帮我写一个 hello world"
      }
    ]
  }
}
```

**流式响应流程:**

1. Agent 立即返回初始响应（确认收到）:
```json
{
  "jsonrpc": "2.0",
  "id": 4,
  "result": {
    "streaming": true,
    "sessionId": "agent-side-session-uuid"
  }
}
```

2. Agent 通过 **notification** 发送流式更新（无 id）:
```json
{"jsonrpc":"2.0","method":"sessionUpdate","params":{"sessionId":"xxx","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"你好"}}}}
```

3. 多个 sessionUpdate 通知持续发送...

4. 最后发送一个带原始 id 的最终响应:
```json
{
  "jsonrpc": "2.0",
  "id": 4,
  "result": {
    "stopReason": "end_turn",
    "response": "完整回复内容"
  }
}
```

**stopReason 值:**

| 值 | 含义 |
|---|---|
| `end_turn` | 正常完成 |
| `stop` | 被中断 |
| `cancelled` | 被取消 |
| `error` | 出错 |

**如果出错，不返回 streaming=true，直接返回错误:**
```json
{
  "jsonrpc": "2.0",
  "id": 4,
  "result": {
    "stopReason": "error",
    "response": "错误描述"
  }
}
```

---

### session/cancel

取消当前正在进行的 prompt。

**Request:**
```json
{
  "jsonrpc": "2.0",
  "id": 5,
  "method": "session/cancel",
  "params": {
    "sessionId": "agent-side-session-uuid"
  }
}
```

**Response:**
```json
{
  "jsonrpc": "2.0",
  "id": 5,
  "result": {
    "cancelled": true
  }
}
```

---

### permissionResponse

用户对权限请求的回复。

**Request:**
```json
{
  "jsonrpc": "2.0",
  "id": 6,
  "method": "permissionResponse",
  "params": {
    "requestId": "req-1",
    "optionId": "allow"
  }
}
```

**Response:**
```json
{
  "jsonrpc": "2.0",
  "id": 6,
  "result": {
    "acknowledged": true
  }
}
```

---

## 通知（agent → aginx，无 id）

### sessionUpdate — 文本块

```json
{
  "jsonrpc": "2.0",
  "method": "sessionUpdate",
  "params": {
    "sessionId": "xxx",
    "update": {
      "sessionUpdate": "agent_message_chunk",
      "content": {
        "type": "text",
        "text": "部分文本内容"
      }
    }
  }
}
```

### sessionUpdate — 工具调用开始

```json
{
  "jsonrpc": "2.0",
  "method": "sessionUpdate",
  "params": {
    "sessionId": "xxx",
    "update": {
      "sessionUpdate": "tool_call",
      "toolCallId": "tc-1",
      "title": "Read(file.rs)",
      "status": "in_progress",
      "kind": "Read"
    }
  }
}
```

### sessionUpdate — 工具调用更新

```json
{
  "jsonrpc": "2.0",
  "method": "sessionUpdate",
  "params": {
    "sessionId": "xxx",
    "update": {
      "sessionUpdate": "tool_call_update",
      "toolCallId": "tc-1",
      "status": "completed"
    }
  }
}
```

**ToolCallStatus:** `in_progress` | `completed` | `failed`

### requestPermission — 请求用户授权

```json
{
  "jsonrpc": "2.0",
  "method": "requestPermission",
  "params": {
    "requestId": "req-1",
    "description": "允许写入 file.py 吗？",
    "options": [
      {"optionId": "allow", "label": "允许"},
      {"optionId": "deny", "label": "拒绝"}
    ]
  }
}
```

Agent 发送此通知后等待 aginx 通过 `permissionResponse` 方法回复。

---

## 错误处理

所有错误使用 JSON-RPC 2.0 标准错误格式：

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "error": {
    "code": -32603,
    "message": "错误描述"
  }
}
```

标准错误码：

| Code | 含义 |
|------|------|
| -32700 | Parse error（JSON 解析失败）|
| -32600 | Invalid Request |
| -32601 | Method not found |
| -32602 | Invalid params |
| -32603 | Internal error |

---

## aginx.toml 配置

Agent 在 aginx 上的配置示例：

```toml
id = "opencarrier"
name = "OpenCarrier"
agent_type = "process"
version = "1.0.0"
description = "Open-source Agent Operating System"

[command]
path = "opencarrier"
args = ["acp"]    # agent 启动 ACP 模式的子命令

[capabilities]
chat = true
streaming = true
permissions = true
```

`protocol` 字段省略时默认为 `"acp"`，即 agent 原生说 ACP。

---

## 最小实现清单

Agent 实现接入 aginx 需要支持：

| 方法/通知 | 必须 | 说明 |
|-----------|------|------|
| `initialize` | ✅ | 握手 |
| `session/new` | ✅ | 创建会话 |
| `session/prompt` | ✅ | 接收消息，返回流式响应 |
| sessionUpdate (text) | ✅ | 发送文本块 |
| 最终响应 (stopReason) | ✅ | 结束流式 |
| `session/load` | 推荐 | 恢复已有会话 |
| `session/cancel` | 推荐 | 取消当前操作 |
| requestPermission | 可选 | 请求用户授权 |
| permissionResponse | 可选 | 接收授权回复 |
| sessionUpdate (tool_call) | 可选 | 显示工具调用 |
| `listConversations` | 可选 | 由 aginx 持久化存储兜底 |
| `getMessages` | 可选 | 由 aginx 持久化存储兜底 |
