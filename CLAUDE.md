# Aginx 开发文档

## 设计理念

### 零配置启动
Aginx 的核心理念是**零配置启动**：
- 服务器端无需任何预先配置
- 所有配置通过 App 完成
- 注册即用

### 内置 Admin Agent
每个 Aginx 实例都有一个内置的 **Admin Agent**，它是一个 LLM 驱动的智能代理：

```
┌─────────────────────────────────────────────┐
│              Admin Agent                     │
│         (内置，LLM + Shell 工具)             │
├─────────────────────────────────────────────┤
│  能力:                                       │
│  • 执行 shell 命令                           │
│  • 分析命令输出和错误                         │
│  • 讨论问题和解决方案                         │
│  • 帮助安装/配置其他 Agent                    │
│  • 管理服务器                                 │
└─────────────────────────────────────────────┘
```

### LLM 配置（BYOK - 用户自带 Key）
用户在 App 中配置 LLM，6 个字段：

```
┌─────────────────────────────────────────────────────┐
│  Provider:  Anthropic                               │
│  API Key:   sk-ant-xxx                              │
│  Endpoint:  https://api.anthropic.com/v1/messages   │
│  Format:    anthropic                               │  ← anthropic / openai / gemini
│  Model:     claude-sonnet-4-6                       │
│  Modality:  chat                                    │  ← 默认 chat
└─────────────────────────────────────────────────────┘
```

**字段说明**：

| 字段 | 说明 | 示例 |
|------|------|------|
| Provider | 名称/标签 | `Anthropic`, `DeepSeek` |
| API Key | 用户自己的 key | `sk-ant-xxx` |
| Endpoint | 完整 URL | `https://api.anthropic.com/v1/messages` |
| Format | API 格式 | `anthropic` / `openai` / `gemini` |
| Model | 模型名 | `claude-sonnet-4-6` |
| Modality | 用途 | `chat`（默认）|

**配置示例**：

```json
// DeepSeek
{
  "provider": "DeepSeek",
  "apiKey": "sk-xxx",
  "endpoint": "https://api.deepseek.com/chat/completions",
  "format": "openai",
  "model": "deepseek-chat",
  "modality": "chat"
}

// Anthropic
{
  "provider": "Anthropic",
  "apiKey": "sk-ant-xxx",
  "endpoint": "https://api.anthropic.com/v1/messages",
  "format": "anthropic",
  "model": "claude-sonnet-4-6",
  "modality": "chat"
}
```

**关键点**：
- 用户用自己的 API Key（BYOK）
- Endpoint 要写完整 URL
- Format 决定请求格式（anthropic/openai/gemini）
- Modality 默认 chat
- 配置保存在 Aginx 端（`~/.aginx/config.json`）

### Agent 安装流程
```
1. App 连接 Aginx
2. App 配置 LLM (BYOK: 用户填入自己的 API Key)
3. 使用内置 Admin Agent 对话
4. Admin Agent 帮助安装新 Agent
   • 执行安装命令
   • 遇到问题可以讨论解决
   • 配置 aginx.toml 文件
5. 新 Agent 安装到 ~/.aginx/agents/
6. 扫描发现、注册使用
```

### 目录结构
```
~/.aginx/
├── config.json        # Aginx 配置（含 LLM API Key）
├── agents/            # 安装的 Agent 配置
│   └── my-agent/
│       └── aginx.toml
└── data/              # 其他数据
```

---

## 服务器信息

- **SSH**: `ssh 86quan` (ubuntu@106.75.32.216)
- **应用目录**: `/data/www/aginx/`
- **API 目录**: `/data/www/aginx-api/`
- **Relay 目录**: `/data/www/aginx-relay/`

## 部署方式

```bash
# 同步代码到服务器
rsync -avz --exclude 'target' --exclude '.git' -e ssh ./ 86quan:/data/www/aginx/

# SSH 登录并编译
ssh 86quan "cd /data/www/aginx && cargo build --release"

# 重启服务
ssh 86quan "pkill -f 'target/release/aginx' ; nohup /data/www/aginx/target/release/aginx > /data/www/aginx/aginx.log 2>&1 &"
```

## 项目结构

```
aginx/
├── src/
│   ├── main.rs           # 入口
│   ├── config/           # 配置管理
│   ├── agent/            # Agent 管理
│   │   ├── manager.rs    # Agent 加载和管理
│   │   ├── discovery.rs  # Agent 发现（扫描 aginx.toml）
│   │   └── session.rs    # 会话管理
│   ├── relay/            # Relay 客户端
│   ├── server/           # 直连服务器
│   └── protocol/         # JSON-RPC 协议
├── examples/
│   ├── test_client_relay.rs    # 测试客户端
│   └── test_session.rs         # 会话管理测试
└── Cargo.toml
```

## 常用命令

### 本地开发
```bash
# 运行
cargo run --bin aginx -- -v

# 测试
cargo run --example test_session agent://rcs0aj94.relay.yinnho.cn
```

### 服务器
```bash
# 查看日志
ssh 86quan "tail -f /data/www/aginx/aginx.log"

# 查看进程
ssh 86quan "ps aux | grep aginx"

# 重启服务
ssh 86quan "pkill -f aginx ; nohup /data/www/aginx/target/release/aginx > /data/www/aginx/aginx.log 2>&1 &"
```
