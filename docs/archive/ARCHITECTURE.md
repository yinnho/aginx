# Aginx 架构设计文档

## 核心理念

**Aginx 是一个透明代理，aginx 自己实现通用的 ACP wrapper，通过配置文件适配不同的 Agent CLI。**

```
┌─────────────────────────────────────────────────────────────┐
│                        App (手机)                          │
│              TCP + JSON-RPC (扩展 ACP 协议)                │
└─────────────────────────┬─────────────────────────────────┘
                          │
┌─────────────────────────▼─────────────────────────────────┐
│                    Aginx (Rust)                            │
├─────────────────────────────────────────────────────────────┤
│  • 标准 ACP 协议实现                                        │
│  • 通用 CLI 调用框架                                        │
│  • 通用 Session 存储管理                                    │
│  • 配置文件驱动                                            │
└─────────────────────────┬─────────────────────────────────┘
                          │
┌─────────────────────────▼─────────────────────────────────┐
│                  CLI Agent (外部)                         │
│               Claude, Kimi, Qwen...                       │
├─────────────────────────────────────────────────────────────┤
│  • 只需提供 CLI 命令                                        │
│  • Aginx 通过配置适配不同 Agent                            │
└─────────────────────────────────────────────────────────────┘
```

---

## 一、协议设计

### 1.1 协议分层

```
┌─────────────────────────────────────────────────────────────┐
│              ACP 扩展方法 (Aginx 自定义)                    │
├─────────────────────────────────────────────────────────────┤
│  • listConversations   - 列出会话                          │
│  • getMessages         - 获取消息历史                       │
│  • deleteConversation  - 删除会话                          │
└─────────────────────────────────────────────────────────────┘
                          ↑
┌─────────────────────────────────────────────────────────────┐
│              ACP 核心方法 (Agent Client Protocol 标准)       │
├─────────────────────────────────────────────────────────────┤
│  • initialize          - 初始化握手                        │
│  • session/new         - 创建会话                          │
│  • session/prompt      - 发送消息（流式）                  │
│  • session/update      - 流式更新 (notification)           │
│  • session/cancel      - 取消                              │
└─────────────────────────────────────────────────────────────┘
```

**说明**：
- **核心方法**：标准 ACP 协议，Agent 适配器需要实现
- **扩展方法**：Aginx 自定义，满足 App 需求

### 1.2 传输层

- **协议**：JSON-RPC 2.0
- **传输**：TCP Socket (App ↔ Aginx) + Stdio (Aginx ↔ Agent CLI)

---

## 二、配置设计

### 2.1 配置文件位置

```
~/.aginx/
├── config.toml           # 全局配置
└── agents/
    ├── claude.toml      # Claude Agent 配置
    ├── kimi.toml        # Kimi Agent 配置
    └── ...
```

### 2.2 Agent 配置结构

```toml
# ~/.aginx/agents/claude.toml

# Agent 基本信息
id = "claude"
name = "Claude"
description = "Anthropic's coding assistant"

# CLI 命令配置
[command]
path = "claude"
args = ["--print", "--output-format", "stream-json"]
env_remove = ["CLAUDECODE"]

# 会话存储配置 (Aginx 负责管理)
[session]
require_workdir = true
storage_dir = "~/.claude/projects"
file_pattern = "{session_id}.jsonl"
```

```toml
# ~/.aginx/agents/kimi.toml

id = "kimi"
name = "Kimi"
description = "Kimi AI assistant"

[command]
path = "kimi"
args = ["chat", "--output", "json"]

[session]
require_workdir = false
storage_dir = "~/.kimi/sessions"
file_pattern = "{session_id}.json"
```

### 2.3 配置字段说明

| 字段 | 类型 | 说明 |
|------|------|------|
| `id` | String | Agent 唯一标识 |
| `name` | String | 显示名称 |
| `description` | String | 描述 |
| `command.path` | String | CLI 命令路径 |
| `command.args` | Array | CLI 参数 |
| `command.env_remove` | Array | 需要移除的环境变量 |
| `session.require_workdir` | Boolean | 是否需要工作目录 |
| `session.storage_dir` | String | 会话存储目录 |
| `session.file_pattern` | String | 文件名模式 |

---

## 三、Session 存储

### 3.1 存储结构

```
~/.aginx/agents/claude/
└── sessions/
    ├── session1.jsonl
    ├── session2.jsonl
    └── ...
```

### 3.2 会话文件格式

```json
{
  "id": "sess_xxx",
  "agent_id": "claude",
  "workdir": "/path/to/workdir",
  "created_at": "2026-03-30T10:00:00Z",
  "updated_at": "2026-03-30T11:00:00Z",
  "last_message": "Hello, how are you?",
  "claude_session_id": "xxx",
  "messages": [
    {
      "role": "user",
      "content": "Hello",
      "timestamp": "2026-03-30T10:00:00Z"
    },
    {
      "role": "assistant",
      "content": "Hi!",
      "timestamp": "2026-03-30T10:00:01Z"
    }
  ]
}
```

### 3.3 存储管理

- **Aginx 负责**：创建、读取、更新、删除会话
- **Agent 不感知**：Aginx 调用 CLI 时传入必要参数，CLI 自己维护状态

---

## 四、ACP 方法定义

### 4.1 标准 ACP 核心方法

| 方法 | 方向 | 参数 | 返回 |
|------|------|------|------|
| `initialize` | 请求→响应 | `{protocolVersion, clientInfo}` | `{serverInfo, capabilities}` |
| `session/new` | 请求→响应 | `{cwd?}` | `{sessionId}` |
| `session/prompt` | 请求→响应 | `{sessionId, prompt}` | `{stopReason}` |
| `session/cancel` | 请求→响应 | `{sessionId}` | `{cancelled}` |

**Notification**：
| 方法 | 说明 |
|------|------|
| `session/update` | 流式更新（`agent_message_chunk`, `stop_reason` 等） |

### 4.2 Aginx 扩展方法

| 方法 | 方向 | 说明 |
|------|------|------|
| `listConversations` | 请求→响应 | 列出所有会话 |
| `getMessages` | 请求→响应 | 获取指定会话的消息历史 |
| `deleteConversation` | 请求→响应 | 删除指定会话 |

---

## 五、与 acpx 的关系

### 5.1 acpx 是什么

- **acpx** 是一个 Node.js CLI 工具，实现了对多种 Agent 的标准 ACP 支持
- 内置 Agent Registry：Claude, Codex, Kimi, Qwen 等
- 通过 npm 安装 adapter：`npx -y @agentclientprotocol/claude-agent-acp`

### 5.2 Aginx vs acpx

| 对比 | acpx | Aginx |
|------|------|-------|
| 语言 | Node.js | Rust |
| 用户 | 命令行调用 | App 远程调用 |
| 通信 | 命令行参数 | TCP Socket |
| Session 存储 | 内置 (`~/.acpx/sessions/`) | 可配置 |
| Agent 支持 | 通过 npm adapter | 通过配置文件 + 通用 wrapper |

### 5.3 Aginx 不依赖 acpx

**Aginx 自己实现通用 wrapper**，不需要：
- Node.js
- npm
- acpx CLI

**Aginx 只调用 Agent 的 CLI 命令**，通过配置适配不同 Agent。

---

## 六、实现要点

### 6.1 通用 Wrapper 设计

```rust
// 核心结构
struct AgentWrapper {
    id: String,
    name: String,
    command: CommandConfig,
    session: SessionConfig,
}

// CLI 调用
async fn call_cli(&self, args: &[String]) -> Result<Output> {
    let mut cmd = Command::new(&self.command.path);
    cmd.args(&self.command.args)
       .args(args);
    // ...
}

// Session 存储
async fn save_session(&self, session: &Session) -> Result<()>;
async fn load_session(&self, id: &str) -> Result<Option<Session>>;
async fn delete_session(&self, id: &str) -> Result<()>;
```

### 6.2 流式响应处理

```rust
async fn prompt(&self, session_id: &str, message: &str) -> impl Stream<Item = Update> {
    // 1. 调用 CLI
    // 2. 解析流式输出
    // 3. 转换为 session/update notification
    // 4. 返回流
}
```

### 6.3 配置加载

```rust
fn load_agent_config(path: &Path) -> Result<AgentConfig> {
    let content = std::fs::read_to_string(path)?;
    let config: AgentConfig = toml::from_str(&content)?;
    Ok(config)
}
```

---

## 七、与 App 的交互

### 7.1 App 请求流程

```
1. App 连接 Aginx (TCP 端口 86)
2. App 发送 initialize 请求
3. App 调用扩展方法：
   - listConversations → 列出所有会话
   - getMessages → 获取消息历史
   - deleteConversation → 删除会话
4. App 调用核心方法：
   - session/new → 创建新会话
   - session/prompt → 发送消息（流式）
   - session/cancel → 取消
```

### 7.2 App 请求示例

```json
// 初始化
{"jsonrpc": "2.0", "method": "initialize", "params": {"protocolVersion": 1, "clientInfo": {"name": "app", "version": "1.0"}}, "id": 1}

// 列出会话
{"jsonrpc": "2.0", "method": "listConversations", "params": {"agentId": "claude"}, "id": 2}

// 获取消息
{"jsonrpc": "2.0", "method": "getMessages", "params": {"sessionId": "sess_xxx"}, "id": 3}

// 发送消息
{"jsonrpc": "2.0", "method": "session/prompt", "params": {"sessionId": "sess_xxx", "prompt": [{"type": "text", "text": "Hello"}]}, "id": 4}
```

---

## 八、总结

1. **Aginx 是透明代理**：不解析 Agent 内容，只转发
2. **标准 ACP 协议**：核心方法遵循 ACP 标准
3. **配置驱动**：不同 Agent 只需不同配置文件
4. **通用 Wrapper**：Aginx 自己实现，不依赖外部 npm 包
5. **Session 自己管理**：存储在 `~/.aginx/agents/<agent>/sessions/`

---

## 九、实现进度

### 已完成

| 任务 | 状态 | 说明 |
|------|------|------|
| 修改 AgentConfig | ✅ | 添加 `SessionConfig` 结构，含 `storage_dir`, `file_pattern` |
| 删除 Claude 硬编码 | ✅ | 删除 `read_claude_conversation_messages` 函数 |
| 重命名 ACP 方法 | ✅ | `newSession` → `session/new`, `prompt` → `session/prompt` |
| 实现 Session 消息存储 | ✅ | 添加 `SessionData` 结构，`get_session_messages` 从 aginx 存储读取 |
| 实现 getMessages | ✅ | `get_session_messages` 返回 aginx 存储的消息历史 |
| 通用化 CLI Wrapper | ✅ | 统一使用 AcpAgentProcess，移除 Session 中的 legacy 代码 |
| 移除 AgentType 枚举 | 待 | 统一使用 Process 类型 |

### 待完成

| 任务 | 优先级 | 说明 |
|------|--------|------|
| 移除 AgentType 枚举 | 中 | 统一使用 Process 类型 |
| 更新配置文件 | 低 | 更新 aginx.toml 示例 |

### 代码改动

- `src/config/mod.rs`: 添加 `SessionConfig`
- `src/acp/handler.rs`: 删除 Claude 硬编码，重命名方法
- `src/agent/session.rs`: 添加 `SessionData`, `SessionMessage`, `get_session_messages`, `append_message`
- `src/agent/manager.rs`: 更新 `require_workdir` 引用

---

## 版本

- 日期：2026-03-30
- 版本：v1.0
- 更新：2026-03-30 14:30 - 添加实现进度
