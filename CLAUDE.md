# Aginx 开发文档

## 服务器信息

- **Relay 服务器**: `root@111.170.156.43` (relay.yinnho.cn)
- **应用目录**: `/data/www/aginx/`
- **Relay 目录**: `/data/www/aginx-relay/`

## 部署方式

```bash
# 同步代码到服务器
rsync -avz --exclude 'target' --exclude '.git' \
  ./ root@111.170.156.43:/data/www/aginx/

# SSH 登录并编译
ssh root@111.170.156.43 "cd /data/www/aginx && cargo build --release"

# 重启服务
ssh root@111.170.156.43 "pkill -f 'target/release/aginx' ; nohup /data/www/aginx/target/release/aginx > /data/www/aginx/aginx.log 2>&1 &"
```

## 项目结构

```
aginx/
├── src/
│   ├── main.rs           # 入口
│   ├── config/           # 配置管理
│   ├── agent/            # Agent 管理
│   │   ├── manager.rs    # Agent 加载和管理
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
ssh root@111.170.156.43 "tail -f /data/www/aginx/aginx.log"

# 查看进程
ssh root@111.170.156.43 "ps aux | grep aginx"

# 重启服务
ssh root@111.170.156.43 "pkill -f aginx ; nohup /data/www/aginx/target/release/aginx > /data/www/aginx/aginx.log 2>&1 &"
```
