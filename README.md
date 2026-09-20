# DGCU Portal

DGCU 校园网客户端。当前工作区只保留两个 Rust package：

```text
portal-cli   engine、daemon、IPC 和 CLI 命令
portal-gui   配置界面、托盘、日志和 daemon 控制器
```

认证、网络健康、Portal/Unify、代拨、会话和重试代码都在 `portal-cli` 的 library 中，`portal-cli` 二进制把它作为同一 package 的 engine 使用。GUI 通过本地 IPC 连接 `portal-cli run --daemon`。

## 命令

```bash
cargo run -p portal-cli -- run --daemon
cargo run -p portal-cli -- up
cargo run -p portal-cli -- down <session-id>
cargo run -p portal-cli -- status
cargo run -p portal-cli -- sessions
cargo run -p portal-cli -- config show
cargo run -p portal-cli -- config set interface en0
cargo run -p portal-cli -- config set refresh 5s
cargo run -p portal-cli -- config set auto-redial on
cargo run -p portal-cli -- config set traffic off
cargo run -p portal-cli -- logs
cargo run -p portal-cli -- diagnose interfaces
```

密码不通过命令行参数传递。`up` 通过本地 IPC 把交互输入交给 daemon；GUI 配置会把凭据写入系统凭据库。

daemon 的本地 IPC 使用 `portal-cli.sock`。GUI 首次需要认证时会拉起同包内的 `portal-cli run --daemon`，GUI 关闭后 daemon 仍继续运行。

## 认证行为

留空 Portal URL 时，daemon 先按 DGCU CMCC 模板构造 `main/nasid/4/` 入口，读取表单并执行 Unify 认证。模板失败且 `probe_enabled` 开启时，才回退到公共 HTTP 探测。Portal 参数使用所选网卡当前 IPv4/MAC，`paip` 固定为 `172.18.100.65`；后台接口使用 LFRadius `home.php` API。

默认监控策略：

```text
network_monitor = on
session_monitor  = on
traffic_monitor  = off
```

GUI 桌面配置可以开启后台流量展示；OpenWrt/无 GUI 运行默认不计算流量。后台刷新策略支持 1 秒、2 秒、5 秒、随机 1-10 秒、1 分钟和禁止刷新。

## GUI

```bash
pnpm --dir crates/portal-gui/frontend install --frozen
pnpm --dir crates/portal-gui/frontend test
pnpm --dir crates/portal-gui/frontend build
cargo run -p portal-gui
```

GUI 负责账号、网卡、刷新、流量展示、日志查看、托盘和桌面服务配置。关闭主窗口只隐藏到托盘；退出 GUI 不会停止 daemon。停止后台服务是单独操作。

账户保存方式有三种：系统凭证、配置文件明文、仅一次会话。默认使用系统凭证；配置文件明文只建议用于测试或没有可用系统凭据库的设备；仅一次会话只在内存中使用密码，结束后释放。

## 构建

```bash
pnpm --dir crates/portal-gui/frontend build
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release -p portal-cli -p portal-gui --features portal-gui/custom-protocol --locked
python3 scripts/package_macos.py --output target/packages
```

macOS 包中包含：

```text
DGCU Portal.app
portal-cli
```

OpenWrt 使用 `portal-cli run --daemon`，由 procd 负责拉起和停止；桌面系统服务也只启动 `portal-cli run --daemon`。

OpenWrt 的 procd 脚本位于 `packaging/openwrt/etc/init.d/portal-cli`，安装后使用：

```bash
/etc/init.d/portal-cli enable
/etc/init.d/portal-cli start
/etc/init.d/portal-cli stop
```

## 目录

```text
crates/portal-cli/src/
├── lib.rs          # engine library
├── controller.rs   # 状态、重试和重拨
├── cmcc.rs         # CMCC/Unify Portal
├── network.rs      # 网卡 IPv4/MAC 和本地网络上下文
├── logging.rs      # 脱敏日志
├── daemon.rs       # daemon IPC 服务
├── ipc.rs          # IPC 请求/响应
└── bin/portal-cli.rs

crates/portal-gui/
├── src/main.rs     # Tauri IPC 客户端和桌面服务控制
└── frontend/       # Vue + miuix-vue
```

旧的 `portal-core` crate、旧 CLI 入口和旧命令别名已经删除，不提供兼容迁移。
