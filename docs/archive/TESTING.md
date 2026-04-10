# 本地测试指南

## ACP Stdio 模式测试

### 1. 启动 aginx ACP 模式

```bash
# 使用 release 版本
./target/release/aginx acp --stdio --verbose
```

这会以 JSON-RPC over stdin/stdout 的方式运行，适合被 IDE 或测试脚本调用。

### 2. 测试脚本

创建测试脚本 `test_acp.sh`:

```bash
#!/bin/bash

# 启动 aginx ACP 模式
AGINX="./target/release/aginx acp --stdio"

# 测试 1: 初始化
init_request='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"0.15.0","clientInfo":{"name":"test","version":"1.0.0"}}}'
echo "$init_request" | $AGINX

# 测试 2: 创建 Session
new_session_request='{"jsonrpc":"2.0","id":2,"method":"newSession","params":{"cwd":"/tmp","_meta":{"agentId":"claude"}}}'
echo "$new_session_request" | $AGINX

# 测试 3: 发送 prompt（触发权限请求）
# 需要先获取 sessionId，这里假设为 test-session
prompt_request='{"jsonrpc":"2.0","id":3,"method":"prompt","params":{"sessionId":"test-session","prompt":[{"type":"text","text":"请读取 ~/.bashrc 文件"}]}}'
echo "$prompt_request" | $AGINX

# 测试 4: 权限响应
permission_response='{"jsonrpc":"2.0","id":4,"method":"permissionResponse","params":{"sessionId":"test-session","outcome":{"outcome":"selected","optionId":"1"}}}'
echo "$permission_response" | $AGINX
```

### 3. 交互式测试

```bash
# 启动 aginx
./target/release/aginx acp --stdio --verbose

# 然后在终端输入 JSON-RPC 请求（每行一个）
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"0.15.0","clientInfo":{"name":"test","version":"1.0.0"}}}

{"jsonrpc":"2.0","id":2,"method":"newSession","params":{"cwd":"/tmp"}}

# 使用返回的 sessionId 发送 prompt
{"jsonrpc":"2.0","id":3,"method":"prompt","params":{"sessionId":"sess_xxx","prompt":[{"type":"text","text":"读取 /etc/passwd"}]}}
```

### 4. 使用 netcat/telnet 测试 TCP 直连模式

如果想测试 TCP 模式（非 stdio）：

```bash
# 终端 1: 启动直连模式
./target/release/aginx --mode direct --port 8086 --verbose

# 终端 2: 使用 nc 发送 ACP 请求
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"0.15.0","clientInfo":{"name":"test","version":"1.0.0"}}}' | nc localhost 8086
```

### 5. 查看详细日志

```bash
# 使用 --verbose 查看 INFO 级别日志
./target/release/aginx acp --stdio --verbose

# 使用 --debug 查看 DEBUG 级别日志（包含 ACP 消息内容）
./target/release/aginx acp --stdio --debug
```

## 预期输出

### 初始化成功
```json
{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"0.15.0","agentCapabilities":{"loadSession":true,...},"agentInfo":{"name":"aginx","version":"0.1.0"}}}
```

### 创建 Session 成功
```json
{"jsonrpc":"2.0","id":2,"result":{"sessionId":"sess_abc123"}}
```

### Prompt 返回（需要权限时）
```json
{"jsonrpc":"2.0","method":"requestPermission","params":{"requestId":"perm_xxx","sessionId":"sess_abc123","description":"Allow Read(/etc/passwd)?","toolCall":{"toolCallId":"tc_xxx","title":"Read(/etc/passwd)"},"options":[{"optionId":"1","label":"Allow this time","kind":"allow_once"},...]}}
{"jsonrpc":"2.0","id":3,"result":{"stopReason":"permission_required","permissionRequest":{"requestId":"perm_xxx","description":"Allow Read(/etc/passwd)?","options":...}}}
```

### 权限响应后
```json
{"jsonrpc":"2.0","id":4,"result":{"stopReason":"end_turn"}}
```

## 故障排除

### Claude CLI 未找到
```
Failed to spawn claude: No such file or directory
```
**解决**: 确保 `claude` 命令在 PATH 中，或安装在 `~/.npm-global/bin/claude`

### PTY 创建失败
```
Failed to open PTY: ...
```
**解决**: 检查系统是否支持 PTY（macOS/Linux 通常支持）

### 权限不触发
如果发送 prompt 后没有收到 `requestPermission` 通知：
1. 检查 `--verbose` 日志中的 ACP 消息
2. 确认工具调用被正确解析（`tool_use` 事件）
3. 验证工具名是否需要权限（Read/Glob/Grep 默认不需要）

## 与 App 集成测试

App 端需要：
1. 启动 aginx 进程: `./target/release/aginx acp --stdio`
2. 写入 JSON-RPC 请求到 stdin
3. 从 stdout 读取响应
4. 处理 `requestPermission` 通知，显示对话框
5. 用户选择后发送 `permissionResponse`

参考代码: `examples/test_acp_interactive.rs` (如需要可创建)
