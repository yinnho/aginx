# ACP — aginx wire 协议（立法版）

**本文档是 aginx 三层 wire 协议的唯一权威规范**（2026-08-23 立法，取代旧版
ACP.md 的"标准 ACP 网关"叙事）。实现与文档由**金样本测试**互锁：三端仓
（aginx 网关 / aginx-carrier 桥 / agc 客户端）各自的测试解析本文档 §6 内嵌
样本，改协议不同批改文档+三端 = 测试红。

**变更铁律**：改任何一层协议 = 同一批改本文档 + 全部说话的端 + 跑金样本测试。
agc 是唯一现役客户端，弄坏 agc = 弄坏全网。

## 0. 三层总览

```
客户端(agc)                relay(aginx-relay)               网关(aginx)              agent 进程(aginx-carrier 桥)
    │  ──第0层 relay 帧──▶      │                             │                        │
    │  connect/connected        │  ◀── register/registered ── │                        │
    │                           │      (网关常驻注册)          │                        │
    │  ══第1层 外部协议(经relay管道)═════════════════════════▶ │ ──第2层 内部 ACP(stdio)─▶ │
    │  initialize / prompt      │      (定向:clientId/广播)    │  initialize/session/*  │
    │  ◀─ chunk… + 最终result ──│                              │  ◀─ session/update… ── │
```

| 层 | 说话双方 | 传输 | 内容 |
|---|---|---|---|
| 第 0 层 | 客户端/网关 ↔ relay | TLS TCP `:8443`（SNI=裸 relay 域名），ndjson | 连接管理：register/connect/ping-pong/data 路由帧 |
| 第 1 层 | 客户端 ↔ 网关 | 同上连接内的 JSON-RPC（Direct 模式则裸 TCP `:86`） | initialize/bindDevice/listAgents/prompt/chunk |
| 第 2 层 | 网关 ↔ agent 进程 | 子进程 stdio，ndjson | ACP JSON-RPC：initialize/session/new/session/prompt + 借用扩展 |

**地址契约**：`agent://<gateway-id>.relay.<domain>/<agent>`。`<gateway-id>.relay.<domain>`
是逻辑地址，**从不做 DNS 解析**——两端都拨裸 relay 域名，靠第 0 层消息里的 id 路由。
Direct 模式地���为 `agent://<host>:<port>/<agent>`。

---

## 1. 第 0 层：relay 帧

所有消息单行 JSON，`"type"` 标签区分。心跳 `{"type":"ping"}` / `{"type":"pong"}`
可随时插入任意方向，**协议循环必须跳过**。

### 1.1 网关侧（register）

网关常驻，断线重连，每连接生命周期内注册一次：

| 方向 | 消息 | 说明 |
|---|---|---|
| 网关→relay | `register {id, token}` | id=网关 id；token=relay secret（缺失被拒） |
| relay→网关 | `registered {id, url}` | url=逻辑地址 `agent://<id>.relay.<domain>` |
| relay→网关 | `error {message}` | 注册失败（secret 错/id 冲突） |

### 1.2 客户端侧（connect）

客户端短连接，一次 prompt 一连接：

| 方向 | 消息 | 说明 |
|---|---|---|
| 客户端→relay | `connect {target, token}` | target=目标网关 id；token=relay secret |
| relay→客户端 | `connected {client_id}` | 分配 client_id（形如 `c_<hex>`） |
| relay→客户端 | `error {message}` | 连接失败 |

### 1.3 数据路由（不对称，务必记住）

| 方向 | 封装 | 规则 |
|---|---|---|
| 客户端→网关 | 客户端写**裸 JSON-RPC 行**，relay 包成 `data {client_id, data}` 转发 | 网关收到的一律是 `data` 帧 |
| 网关→客户端（定向） | 网关写 `{clientId, ...响应}`，relay **剥掉 clientId** 后原样转发 | 带响应 id 的最终响应走这条 |
| 网关→客户端（广播） | 网关写**裸 JSON-RPC 行**（无 clientId），relay 广播给该网关全部客户端 | chunk 通知走这条；单客户端网关等价于定向 |

### 1.4 断连通知

relay→网关：`disconnected {client_id}`；网关随之清掉该客户端的鉴权态。

---

## 2. 第 1 层：外部协议（客户端 ↔ 网关）

JSON-RPC 2.0 over ndjson（经第 0 层管道或 Direct TCP）。单行上限 **128MB**
（借用轮 ticket/materials/files 走单行 JSON）。

### 2.1 initialize

连接后第一个请求。**请求里的 protocolVersion 网关不校验**（客户端可发任意
字符串），响应恒为整数 `1`。鉴权 token 可放 `params._meta.authToken` 或
`params.token`（后者兼容旧客户端）。

### 2.2 鉴权三层（网关侧闸门）

| 级别 | 来源 | 权限 |
|---|---|---|
| public | `access = public` 模式，无 token | 全放行（伪 Bound） |
| Bound | `bindDevice` 配对得到的设备 token | 全放行（主人本人设备） |
| Authorized | `aginx auth` 签发的 client token / JWT | 按 claims 白名单（方法/agent/system） |

未鉴权（private/protected 且无 token）只放行 `initialize` 和 `bindDevice`。

### 2.3 bindDevice

参数 camelCase：`pairCode` / `deviceName` → `{deviceId, deviceName, token}`。
配对码由网关侧 `aginx pair` 生成，一次性。

### 2.4 listAgents / ping

`listAgents`（别名 `agents/list`）→ `{agents: [{id, name, description?}]}`
（Authorized 按 agent 白名单过滤）。`ping` → `{pong: true}`。

### 2.4.1 sessions/list（会话列表）

`{agent}` → `{sessions: [{sessionId, title, lastTs, turns}]}`（按 lastTs 倒序）。
**事实源 = 网关台账**：prompt 成功轮以翻译器收割的 agent 真会话 id（§2.5）记
`{注册名, sessionId, title=首句截断, lastTs, turns=经手轮数}`，只记网关经手的
轮，不读 agent 私有存储（claude 会话库等一律不碰——会话属 agent 侧，网关只记
自己经手的账）。`sessionId` 即下轮 prompt 的续接 id。raw 方言（无翻译器收割）
自然为空表——不报错。未知 `agent` → `-32602`。读方法，鉴权归 safe 档（同
listAgents）。

### 2.5 prompt

**核心方法**。参数（`agent` 与 `message` 必填）：

| 字段 | 类型 | 说明 |
|---|---|---|
| `agent` | string | 目标 agent id（= agent:// URL 的路径段） |
| `message` | string | 用户消息（扁平字符串，**不是** ContentBlock 数组） |
| `cwd` | string? | 工作目录（须在网关主目录下，否则忽略） |
| `sessionId` | string? | 会话续接 id（透传给 adapter 的 `${SESSION_ID}` 模板） |
| `sessionTicket` | object? | **借用轮触发器**——在场即走无状态借道（见 §4） |
| `materials` | array? | `[{name, contentBase64}]`，随轮进随轮毁 |
| `activeFlow` | string? | 显式 flow 名（按名加载，零 LLM classify） |
| `borrower` | string? | 借用者身份自报（仅无鉴权时被网关采信，防冒名） |

**响应时序（两条铁则）**：

1. **带 id 的立即 ack 永不出网**：内部产生的 `{streaming: true, sessionId}`
   ack 仅作为"流式开始"信号，网关的 relay/Direct 出口见到 `result` 含
   `streaming` 键即吞掉不发。客户端不该期待任何带 id 的中间响应。
2. **最终响应无 id**，经通知通道下发：`{jsonrpc, result: {stopReason, …}}`。

普通轮最终结果：`{stopReason: "endTurn", sessionId?, costUsd?, durationMs?, numTurns?}`。

**`sessionId` 语义（2026-08-24 起）：网关收割的 agent 真会话 id**——CLI
路径经 output 翻译器（§2.8）从 agent 输出收割；无翻译器方言时回显客户端
传入值。客户端把它原样回喂下轮 `prompt.sessionId` 即续接。`costUsd` /
`durationMs` / `numTurns` 是翻译器可选附带的成本/时长/轮数（数值，缺省
省略）。
借道轮最终结果：`{stopReason, streaming: true, sessionId, sessionTicket, files}`
（见 §6 金样本）。

`stopReason` 外部词汇表：`endTurn`（正常）/ `cancelled`（被取消）。其余异常
一律走 JSON-RPC `error` 响应（-32603：进程起不来/超时/退出码非 0）。

### 2.6 chunk 通知（流式文本）

`{"jsonrpc":"2.0","method":"chunk","params":{"text":…,"sessionId?":…}}`，
无 id。**这是外部协议唯一的流式通知**（网关把 agent 侧 session/update 翻译
成它）。逐块到达，客户端拼接即最终文本。

chunk 的 text 是**翻译后的纯文本**：接入包声明 `output`（§2.8）的 CLI
agent，网关按声明把 CLI stdout 行翻译成纯文本（如 claude-stream-json 只发
assistant 文本块）；未声明/`raw` 保持原行直通。

### 2.8 接入包输出声明（output）

`aginx.toml` 顶层 `output` 字段声明该 CLI 的 stdout 方言，网关据此挂翻译
器——**方言知识属于接入包，不属于网关核心**（网关代码零 CLI 字样）：

| 值 | 方言 | 行为 |
|---|---|---|
| `raw`（缺省） | 裸文本行 | 行直通 chunk；result.sessionId 回显客户端传入 |
| `claude-stream-json` | claude `--output-format stream-json --verbose` | assistant 行 text 块 → chunk 纯文本；`type=result` 行收割真 session_id + costUsd/durationMs/numTurns（记入台账 §2.4.1）；`is_error:true` → error 帧 |

### 2.7 借用者身份透传优先级（网关→桥）

`Authorized → client.id`（不可冒充）＞ `public 伪 Bound → 客户端显式
borrower`（friends 名单门交给���侧 `[borrow]` 配置裁决）＞ `真 Bound →
不透传`（桥按 local 处理，不受名单门限制）。

---

## 3. 第 2 层：内部 ACP（网关 ↔ agent 进程）

网关按 agent 的 `aginx.toml` 拉起子进程，stdin/stdout ndjson。**每 prompt
一次性进程**（借用轮贴合此模型：无状态会话真源在票据里，无需常驻）。

### 3.1 双模嗅探（aginx-carrier 桥特有）

桥读 stdin 首行判模式：以 `{` 开头、可解析 JSON 且带 `"jsonrpc"` 键 → ACP
模式；否则整段 stdin 为消息走 **ask 裸文本模式**（stdout 裸文本流式输出、
exit 0=成功/非 0=失败）。两种模式共用同一条 `aginx.toml` command。

### 3.2 ACP 方法集（桥实现）

| 方法 | 请求要点 | 响应 |
|---|---|---|
| `initialize` | `protocolVersion` 整数 1 | `{protocolVersion:1, agentInfo{name,title,version}, agentCapabilities{loadSession:false, promptCapabilities{image:false,audio:false,embeddedContext:true}}, authMethods:[]}` |
| `session/new` | params 可为空（桥忽略 cwd/mcpServers） | `{sessionId}`（进程内 boot kernel + 注册，失败 -32000） |
| `session/prompt` | `{sessionId, prompt:[ContentBlock], sessionTicket?, materials?, activeFlow?, borrower?}` | 见下 |
| `session/set_mode` | 任意 | `null` |
| `session/cancel` | 通知（无 id）`{sessionId}` | 打取消标志，在飞轮尽快回 `stopReason:"cancelled"` |

`prompt` ContentBlock 桥支持 `text` / `resource` / `resource_link` 三种
（image/audio 因能力声明 false 而拒绝，-32602）。

**不带 `sessionTicket`** → 持久 session 轮（`acp:<sessionId>` 标签落主人
DB，向后兼容路径）。
**带 `sessionTicket`** → 无状态借用轮（§4）：轮末响应
`{stopReason:"end_turn", sessionTicket, files}`。

### 3.3 流式通知

`session/update`，`params.update.sessionUpdate` 桥只发 `agent_message_chunk`
（`content.text` 为增量）。标准 ACP 的 tool_call/plan 等未实现——网关的
直通翻译也只消费这一种。

### 3.4 stopReason 词汇（内部 = 标准 ACP）

`end_turn` / `cancelled`。**网关是词汇翻译边界**：内部 `end_turn` 出网变
`endTurn`（§2.5）。

---

## 4. 票据、素材、产物（借用五件套）

### 4.1 SessionTicket（wire 形状 camelCase）

```json
{
  "version": 1,
  "label": "borrow:travel-planner",
  "messages": [{"role": "user", "content": "我叫小明"}],
  "turnSummaries": [],
  "contextWindowTokens": 32000
}
```

- 票据是**会话的唯一真源**，用户侧持久（agc `--save-ticket` 文件 /
  TicketStore）；主人服务器零持久化。
- 消息层滚动截断：超 256KB 预算丢最老的**完整 user+assistant 对**，更早轮次
  语义由 `turnSummaries` 摘要层全量承载（原文窗口 + 摘要全量，同 compaction
  L1 分层）。
- `messages` 元素 = `{role: "system"|"user"|"assistant", content: 文本或块数组}`。

### 4.2 materials（进）/ files（回）——形状对称

```json
{"name": "成绩单.md", "contentBase64": "…"}
```

- 素材落 agent workspace `borrow/<uuid>/materials/<name>`，轮末整目录销毁；
  恶意名（`../`、绝对路径、反斜杠）拒绝。预算单文件 8MiB / 总 32MiB。
- 产物从 `borrow/<uuid>/output/` 收集回流，预算单文件 16MiB / 总 64MiB
  （超限跳过计数告警，不失败）。

### 4.3 桥侧准入与配额（carrier `[borrow]` 配置）

`enabled` 总开关 → `allow_borrowers` 名单（空=不限）→ `max_turns_per_hour`
滑窗（0=不限，**且 0 时连台账写入也跳过**——「不限量但留审计」无配置组合）。
计数单位 = 借用轮（带 ticket 的 `session/prompt` 一次），按 borrower×agent
维度；`borrower` 缺省记 `"local"`（免名单门、照常计数）。三闸拒绝与
materials 体量超限统一报内层 -32002。台账 JSONL 只记
`{ts, borrower, agent}` 元数据。

完整配额语义（三闸分层/身份解析/体量预算不对称/信任模型/运营语义）立法在
`../ARCHITECTURE.md` §5.5——本节只留 wire 契约。

---

## 5. 错误码

**外部（网关出）**：

| 码 | 场景 |
|---|---|
| -32700 | 行解析失败 |
| -32600 | 需要鉴权 / 无效请求 / 配对码错 |
| -32601 | 方法不存在 / agent 不存在 |
| -32602 | params 无效 |
| -32603 | agent 进程 spawn 失败 / 超时 / 退出码非 0 |

**内部（桥出）**：

| 码 | 场景 |
|---|---|
| -32700 | 行解析失败 |
| -32601 | 方法不存在 |
| -32602 | prompt 块类型不支持 / sessionTicket 无效 / materials 无效 |
| -32000 | kernel boot 失败 |
| -32001 | unknown sessionId |
| -32002 | agent 轮失败 / 借用轮失败（含准入、配额、materials 体量拒绝——见 §4.3） |

---

## 6. 金样本（golden samples）

以下样本是**协议的最小可执行规范**。每条以 `<!-- golden: 名字 -->` 标记，
其后紧跟一个 ```json 块。三端测试按名字提取并用自己的类型解析——文档与
实现打架即测试红。样本值（id/名字/密钥占位符）是任意的，**形状与字段名**
是契约。

<!-- golden: relay_register -->
```json
{"type": "register", "id": "qi7o6bj5", "token": "<relay-secret>"}
```

<!-- golden: relay_registered -->
```json
{"type": "registered", "id": "qi7o6bj5", "url": "agent://qi7o6bj5.relay.aginx.net"}
```

<!-- golden: relay_connect -->
```json
{"type": "connect", "target": "qi7o6bj5", "token": "<relay-secret>"}
```

<!-- golden: relay_connected -->
```json
{"type": "connected", "client_id": "c_a1b2c3d4"}
```

<!-- golden: relay_data_to_gateway -->
```json
{"type": "data", "client_id": "c_a1b2c3d4", "data": {"jsonrpc": "2.0", "id": 2, "method": "prompt", "params": {"agent": "travel-planner", "message": "只回：ok"}}}
```

<!-- golden: relay_directed_response -->
```json
{"clientId": "c_a1b2c3d4", "jsonrpc": "2.0", "id": 1, "result": {"protocolVersion": 1, "authenticated": true, "serverInfo": {"name": "aginx", "version": "0.3.1"}}}
```

<!-- golden: relay_broadcast_notification -->
```json
{"jsonrpc": "2.0", "method": "chunk", "params": {"text": "好的", "sessionId": "sess_1a2b3c"}}
```

<!-- golden: external_initialize_request -->
```json
{"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "0.1.0", "clientInfo": {"name": "agc", "version": "0.2.0"}, "_meta": {"authToken": "<device-or-client-token>"}}}
```

<!-- golden: external_initialize_result -->
```json
{"protocolVersion": 1, "authenticated": true, "serverInfo": {"name": "aginx", "version": "0.3.1"}}
```

<!-- golden: external_bind_device_request -->
```json
{"jsonrpc": "2.0", "id": 1, "method": "bindDevice", "params": {"pairCode": "123456", "deviceName": "My Phone"}}
```

<!-- golden: external_bind_device_result -->
```json
{"deviceId": "dev_9f8e", "deviceName": "My Phone", "token": "<device-token>"}
```

<!-- golden: external_prompt_plain_request -->
```json
{"jsonrpc": "2.0", "id": 2, "method": "prompt", "params": {"agent": "travel-planner", "message": "只回：ok"}}
```

<!-- golden: external_prompt_borrowed_request -->
```json
{"jsonrpc": "2.0", "id": 3, "method": "prompt", "params": {"agent": "travel-planner", "message": "读素材里的暗号并写进产物", "sessionTicket": {"version": 1, "label": "borrow:travel-planner", "messages": [{"role": "user", "content": "我叫小明"}, {"role": "assistant", "content": "你好小明"}], "turnSummaries": [], "contextWindowTokens": 32000}, "materials": [{"name": "secret.txt", "contentBase64": "OTUyNwo="}], "activeFlow": "consultation", "borrower": "friend-a"}}
```

<!-- golden: external_chunk_notification -->
```json
{"jsonrpc": "2.0", "method": "chunk", "params": {"text": "好的，我来规划", "sessionId": "sess_1a2b3c"}}
```

<!-- golden: external_final_result_plain -->
```json
{"jsonrpc": "2.0", "result": {"stopReason": "endTurn", "sessionId": "sess_1a2b3c"}}
```

<!-- golden: external_final_result_translated -->
```json
{"jsonrpc": "2.0", "result": {"stopReason": "endTurn", "sessionId": "b8f713a4-ea3b-4d6c-920b-87dc2a0403f0", "costUsd": 0.015, "durationMs": 8400, "numTurns": 1}}
```

<!-- golden: external_sessions_list_request -->
```json
{"jsonrpc": "2.0", "id": 7, "method": "sessions/list", "params": {"agent": "claude"}}
```

<!-- golden: external_sessions_list_result -->
```json
{"sessions": [{"sessionId": "b8f713a4-ea3b-4d6c-920b-87dc2a0403f0", "title": "看一下以前的会话", "lastTs": "2026-08-24T00:03:23Z", "turns": 2}]}
```

<!-- golden: external_final_result_borrowed -->
```json
{"jsonrpc": "2.0", "result": {"stopReason": "endTurn", "streaming": true, "sessionId": "sess_1a2b3c", "sessionTicket": {"version": 1, "label": "borrow:travel-planner", "messages": [{"role": "user", "content": "我叫小明"}, {"role": "assistant", "content": "你好小明"}, {"role": "user", "content": "读素材里的暗号并写进产物"}, {"role": "assistant", "content": "暗号 9527 已写进 advice.md"}], "turnSummaries": [], "contextWindowTokens": 32000}, "files": [{"name": "advice.md", "contentBase64": "IyDpobbkvowKClvpq5jotLkK"}]}}
```

<!-- golden: internal_initialize_request -->
```json
{"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": 1}}
```

<!-- golden: internal_initialize_result -->
```json
{"protocolVersion": 1, "agentInfo": {"name": "travel-planner", "title": "aginx-carrier · travel-planner", "version": "0.1.0"}, "agentCapabilities": {"loadSession": false, "promptCapabilities": {"image": false, "audio": false, "embeddedContext": true}}, "authMethods": []}
```

<!-- golden: internal_session_new_request -->
```json
{"jsonrpc": "2.0", "id": 2, "method": "session/new", "params": {}}
```

<!-- golden: internal_session_new_result -->
```json
{"jsonrpc": "2.0", "id": 2, "result": {"sessionId": "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0"}}
```

<!-- golden: internal_session_prompt_borrowed_request -->
```json
{"jsonrpc": "2.0", "id": 3, "method": "session/prompt", "params": {"sessionId": "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0", "prompt": [{"type": "text", "text": "读素材里的暗号并写进产物"}], "sessionTicket": {"version": 1, "label": "borrow:travel-planner", "messages": [{"role": "user", "content": "我叫小明"}, {"role": "assistant", "content": "你好小明"}], "turnSummaries": [], "contextWindowTokens": 32000}, "materials": [{"name": "secret.txt", "contentBase64": "OTUyNwo="}], "activeFlow": "consultation", "borrower": "friend-a"}}
```

<!-- golden: internal_session_update_notification -->
```json
{"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0", "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "好的，我来规划"}}}}
```

<!-- golden: internal_final_result_borrowed -->
```json
{"jsonrpc": "2.0", "id": 3, "result": {"stopReason": "end_turn", "sessionTicket": {"version": 1, "label": "borrow:travel-planner", "messages": [{"role": "user", "content": "我叫小明"}, {"role": "assistant", "content": "你好小明"}, {"role": "user", "content": "读素材里的暗号并写进产物"}, {"role": "assistant", "content": "暗号 9527 已写进 advice.md"}], "turnSummaries": [], "contextWindowTokens": 32000}, "files": [{"name": "advice.md", "contentBase64": "IyDpobbkvowKClvpq5jotLkK"}]}}
```

<!-- golden: ticket_v1 -->
```json
{"version": 1, "label": "borrow:travel-planner", "messages": [{"role": "user", "content": "我叫小明"}, {"role": "assistant", "content": "你好小明"}], "turnSummaries": [], "contextWindowTokens": 32000}
```

<!-- golden: material_entry -->
```json
{"name": "secret.txt", "contentBase64": "OTUyNwo="}
```

<!-- golden: output_file_entry -->
```json
{"name": "advice.md", "contentBase64": "IyDpobbkvowKClvpq5jotLkK"}
```

---

## 7. 已知过时项与勘误史

- 旧版 ACP.md 的 `session/prompt`（ContentBlock[]）/ `session/load` /
  `session/list` / 权限请求 / 终端方法：**外部协议从未用过**。外部层是扁平
  `prompt`+`message`（§2.5）；ContentBlock 只存在于第 2 层网关→桥方向。
  （2026-08-24 注：新立法的外部方法 `sessions/list`（§2.4.1）与旧内部
  `session/list` 是不同方法——按接入包 output 声明探测 agent 侧会话库。）
- `_aginx/*` 下划线方法名：实现是裸名（`bindDevice`/`listAgents`），无
  下划线前缀。`_aginx/discoverRemote`/`listConversations`/`getMessages`/
  `deleteConversation`/`listDirectory`/`readFile` **不存在**（permission 表
  里的引用是死代码）。
- `aginx acp --stdio` 子命令 v0.3.1 已删；`protocol = "acp"` 的
  AcpStdioAdapter 已随多 adapter 架构退役，现役唯一 adapter 是
  PromptAdapter（每 prompt 一进程）。
- JWT 签发服务 aginx-api 已死；现役 token 签发是网关本地 `aginx auth` 命令。
- initialize `protocolVersion` 字符串 vs 整数之争的裁决：**两层各自为政**——
  外部层请求不校验（客户端随意），响应恒整数 1；内部层双向整数 1。

## 8. 变更流程

1. 改本文件 §1–§5 + §6 金样本；
2. 同批改全部说话端（说话该层的仓）；
3. 三仓 `cargo test` 金样本全绿；
4. agc 冒烟一条真实借道轮（素材进→产物回→票据回喂）才算改完。
