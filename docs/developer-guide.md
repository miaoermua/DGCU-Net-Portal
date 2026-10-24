# 开发者文档

> 对应版本：0.5.13。运行、配置与构建说明。
> 协议事实与抓包依据见[校园网认证协议分析](./campus-network-auth.md)，代码如何实现这些协议（含状态、错误、时序常量与算法）见[校园网认证协议实现](./protocol-implementation.md)。

DGCU 校园网客户端。当前工作区只保留两个 Rust package：

```text
portal-cli   engine、daemon、IPC 和 CLI 命令
portal-gui   配置界面、托盘、日志和 daemon 控制器
```

认证、网络健康、Portal/Unify、代拨、会话和重试代码都在 `portal-cli` 的 library 中，`portal-cli` 二进制把它作为同一 package 的 engine 使用。GUI 通过本地 IPC 连接 `portal-cli run --daemon`，不直接调用认证逻辑；文件级职责见[协议实现·代码地图](./protocol-implementation.md#一代码地图)。

## CLI 命令

所有需要 daemon 的命令都通过 `portal-cli.sock` 转发，CLI 自己不做认证：

```bash
cargo run -p portal-cli -- run --daemon
cargo run -p portal-cli -- up
cargo run -p portal-cli -- down <session-id>
cargo run -p portal-cli -- status
cargo run -p portal-cli -- sessions
cargo run -p portal-cli -- config show
cargo run -p portal-cli -- config set interface en0
cargo run -p portal-cli -- config set paip 172.18.100.65
cargo run -p portal-cli -- config set basip 172.18.100.61
# 恢复为使用认证表单返回值
cargo run -p portal-cli -- config set basip auto
cargo run -p portal-cli -- config set refresh 1m
cargo run -p portal-cli -- config set auto-redial new
cargo run -p portal-cli -- config set jitter low
cargo run -p portal-cli -- config set traffic off
cargo run -p portal-cli -- config set portal-probe off
cargo run -p portal-cli -- config set proxy bypass
cargo run -p portal-cli -- config account
cargo run -p portal-cli -- logs
cargo run -p portal-cli -- diagnose interfaces
cargo run -p portal-cli -- service enable
```

密码不通过命令行参数传递。`up` 与 `config account` 在终端交互输入，密码走 `rpassword` 隐藏输入（`crates/portal-cli/src/bin/portal-cli.rs:76-86`）；`up` 通过本地 IPC 把交互输入交给 daemon，`config account` 会把账号写进设置并强制 `credential_store=system`、密码存进系统凭据库（`bin/portal-cli.rs:232-240`）。

`config set` 支持的键与取值（`bin/portal-cli.rs:94-149`）：

| 键 | 取值 |
|---|---|
| `interface` | 网卡名，空字符串表示自动选择 |
| `paip` | IP 地址 |
| `basip` | IP 地址，或 `auto`（等价于留空） |
| `refresh` | `1m`、`60s`、`off` |
| `jitter` | `low`、`medium`、`high`、`off` |
| `portal-probe` | `on`、`off` |
| `proxy` | `bypass`、`system` |
| `auto-redial` | `off`、`new`、`terminate` |
| `traffic` | `on`、`off` |

### 目前未接入的命令

这些子命令存在但会直接返回错误，不要写进脚本：

| 命令 | 实际行为 |
|---|---|
| `run`（不带 `--daemon`） | 报错“请使用 portal-cli run --daemon”（`bin/portal-cli.rs:163`） |
| `logs --clear` | 报错“日志清理由 GUI 配置完成后执行”；`--follow` 被解析但忽略（`bin/portal-cli.rs:213-216`） |
| `config get <key>` | 与 `config show` 行为相同，都打印整份配置（`bin/portal-cli.rs:225-227`） |
| `config reset` | 报错“配置重置需要明确指定配置文件路径”（`bin/portal-cli.rs:241`） |
| `diagnose backend`/`portal`/`probe`/`routes`/`request` | 报错“该诊断命令尚未接入 daemon 请求链”；只有 `interfaces` 与 `config` 有实现（`bin/portal-cli.rs:275-290`） |

## 配置文件

设置是单个 JSON 文件，路径由 `ProjectDirs::from("net", "dgcu", "portal").config_dir()` 加 `settings.json` 决定（`crates/portal-cli/src/settings.rs:166-170`）。`directories` 在各平台只取其中一部分字段，因此三平台的目录名并不一致：

| 平台 | `settings.json` |
|---|---|
| Linux | `$XDG_CONFIG_HOME/portal/settings.json`，通常 `~/.config/portal/settings.json` |
| macOS | `~/Library/Application Support/net.dgcu.portal/settings.json` |
| Windows | `%APPDATA%\dgcu\portal\config\settings.json` |

写入是原子的：先写同目录的 `settings.tmp` 再 `rename`，unix 下文件权限 `0600`（`settings.rs:230-250`）。明文凭据文件是同目录下的 `credentials`（`settings.rs:258-260`）。

字段与默认值（`settings.rs:114-165`）：

| 字段 | 默认值 | 说明 |
|---|---|---|
| `server` | `http://172.18.100.65/lfradius/` | LFRadius 后台地址（`lib.rs:21`）；必须是 http(s)、不含凭据、query 与 fragment（`lib.rs:250-272`） |
| `auth_url` | `{server}web/admin/login` | 只用于界面上的“打开认证网站”，认证流程不读它（`usePortal.ts:226`、`crates/portal-gui/src/main.rs:298-304`） |
| `probe_url` | `http://captive.apple.com/hotspot-detect.html` | 公共探测的起点，另有两个内置备用探测地址 |
| `paip` | `172.18.100.65` | Portal 侧地址，写进入口 URL；必须是合法 IP（`settings.rs:192-194`） |
| `basip` | 空 | 空表示沿用认证表单返回的隐藏值；填写后才覆盖，必须是合法 IP（`settings.rs:195-199`） |
| `probe_enabled` | `true` | 只在 DGCU 模板表单不可用时才回退公共探测 |
| `refresh_policy` | `one_minute` | `one_minute` / `disabled`（`settings.rs:31-37`） |
| `poll_jitter` | `low` | `low` ±5% / `medium` ±10% / `high` ±20% / `disabled` 0%；旧配置的 `enabled` 会被读成 `low`（`settings.rs:39-48`、`settings.rs:88-96`） |
| `traffic_enabled` | `false` | 关闭时清空速率基线（`controller.rs:440-447`） |
| `interface_name` | 空 | 空表示自动选第一个非 internal、同时有 IPv4 与 MAC 的网卡（`network.rs:120-128`） |
| `bypass_proxy` | `true` | 为 `true` 时 HTTP 客户端 `no_proxy()`（`lib.rs:334-336`） |
| `credential_store` | `system` | `system` / `file` / `memory`（`settings.rs:50-57`） |
| `reconnect_mode` | `disabled` | `disabled` / `new_session` / `terminate_and_reconnect`（`settings.rs:59-66`） |
| `run_mode` | `lightweight` | `standard` / `tray_startup` / `lightweight`（`settings.rs:69-79`） |
| `service_enabled` | `false` | 是否写入登录启动任务 |
| `username` | 空 | `memory` 模式下保存前会被清空（`settings.rs:203-205`） |
| `show_sessions`、`log_enabled` | `false`、`false` | 界面偏好 |
| `theme_mode` | `system` | `system` / `light` / `dark`（`settings.rs:12-19`） |

0.4.7 及以前的配置只有 `tray_startup` 布尔值；缺 `run_mode` 字段时按旧值升级，`true` 对应 `tray_startup`，避免老配置被默认值悄悄换成另一种退出行为（`settings.rs:215-229`）。

## 认证行为

留空 Portal URL 时，daemon 先按 DGCU CMCC 模板构造 `main/nasid/4/` 入口，读取表单并执行 Unify 认证。模板失败且 `probe_enabled` 开启时，才回退到公共 HTTP 探测。Portal 参数使用所选网卡当前 IPv4/MAC，`paip` 默认是 `172.18.100.65`，可按学校修改；`basip` 默认留空并使用认证表单返回值，填写后才覆盖该返回值。后台接口使用 LFRadius `home.php` API。三条入口来源与探测链的约束见[协议实现·第三节](./protocol-implementation.md#三portal-入口从哪里来)。

默认轮询策略：

```text
refresh_policy  = one_minute   # 后台刷新，约 1 分钟一次
reconnect_mode  = disabled     # 掉线重拨，开启后每 5 秒检测一次
traffic_enabled = false        # 后台流量速率
poll_jitter     = low          # ±5%，同时作用于上面两个间隔与重拨退避
```

GUI 桌面配置可以开启后台流量展示；OpenWrt/无 GUI 运行默认不计算流量。后台刷新是一个开关，开启后按服务端约 1 分钟更新；掉线重拨独立按 5 秒检测。掉线重拨模式可以选择禁用、新建后上线或终止后重上。认证设置中的轮询频率抖动可以选择低（±5%）、中（±10%）、高（±20%）或禁用（0%），分别作用于 5 秒掉线检测和 1 分钟后台刷新，重拨退避也使用该等级。两个轮询由 daemon 的两个独立 worker 驱动，时序常量与退避算法见[协议实现·第八节](./protocol-implementation.md#八掉线检测重拨与网络自愈)。

轮询抖动也可以通过 CLI 设置：

```bash
portal-cli config set jitter low
portal-cli config set jitter medium
portal-cli config set jitter high
portal-cli config set jitter off
```

自动重连模式可以选择：

```bash
portal-cli config set auto-redial off
portal-cli config set auto-redial new
portal-cli config set auto-redial terminate
```

daemon 的本地 IPC 使用 `portal-cli.sock`。GUI 首次需要认证时会启动 daemon；如果启用了用户级后台服务，GUI 会先请求 launchd/systemd 启动并等待 IPC 就绪，服务管理器未能实际拉起 daemon 时会自动退回启动同包内的 `portal-cli run --daemon`。GUI 关闭后 daemon 仍继续运行。端点被占用时的让位/接管规则见[协议实现·第十节](./protocol-implementation.md#十ipc-与-daemon)。

## GUI

```bash
pnpm --dir crates/portal-gui/frontend install --frozen
pnpm --dir crates/portal-gui/frontend test
pnpm --dir crates/portal-gui/frontend build
cargo run -p portal-gui
```

前端只有 8 个源文件、没有组件目录：三个页面（网络、设置、关于）都在 `App.vue` 里，状态与动作集中在 `usePortal.ts`，类型在 `types.ts`，另有 `licenses.ts`、`compact.css`、`main.ts`、`miuix-icons.d.ts` 与 `usePortal.test.ts`。

`crates/portal-gui/src/main.rs:459-478` 注册的 Tauri 命令共 18 个：

```text
initial          connect         refresh          snapshot
diagnose         select_session  disconnect       forget
read_logs        clear_logs      list_interfaces  save_preferences
save_settings    open_auth_site  open_repository  open_external
check_update     exit_app
```

除 `list_interfaces`（直接调 `portal_cli::network::list()`）与 `save_settings`/`save_preferences`（直接读写配置文件）外，其余都经 IPC 转发给 daemon。`save_settings` 与 `save_preferences` 保存后都会发 `Request::Reload`，让 daemon 重新加载设置并让已建立的连接跟着变。`check_update` 只比对 GitHub Releases 上的版本号并打开 Releases 页面，不下载也不安装（`crates/portal-gui/src/update.rs`）。

GUI 负责账号、网卡、刷新、流量展示、日志查看、托盘和桌面服务配置。设置里的“运行方式”决定界面进程的启动与关闭行为：

| `run_mode` | 启动 | 关闭窗口 |
|---|---|---|
| `standard`（常规） | 显示窗口 | 收进托盘常驻 |
| `tray_startup`（托盘启动） | 隐藏窗口 | 收进托盘常驻 |
| `lightweight`（轻量模式，默认） | 显示窗口 | 直接退出 GUI 并释放托盘，只留独立的后台服务 |

daemon 是独立进程、认证不受影响，任何模式下都不会因为关掉窗口而中断。运行方式每次关闭窗口时重读设置，保存后即时生效、无需重启（`main.rs:447-458`）；保存时若运行方式变了会重建托盘菜单，免得切到轻量模式后仍留着重复项（`main.rs:287-294`）。关窗后的去向由纯函数 `close_action` 决定（`main.rs:344-349`），只有 `lightweight` 退进程，其余收进托盘。

收进托盘时 macOS 上会一并撤掉 Dock 图标（`main.rs:358-369`）：程序还在跑但没有可见界面，Dock 里挂个点不动的图标只会让人以为卡住；托盘图标不受影响，两者是两套机制。托盘唤回窗口时先把 Dock 图标放回去再显示窗口（`main.rs:371-377`），顺序反了 macOS 会把刚显示的窗口当成后台应用的一部分。非 macOS 平台没有 Dock，`set_dock_visible` 是空操作。`tray_startup` 启动时窗口就是隐藏的，因此启动即撤掉 Dock 图标（`main.rs:434-440`）。

托盘菜单固定三项：“打开主窗口”“进入轻量模式”“退出服务”；`lightweight` 下“进入轻量模式”与关窗重复，菜单中不展示该项（`main.rs:22-35`）。“进入轻量模式”只调用 `app.exit(0)` 结束 GUI 进程，daemon 与认证不受影响，且不会写回 `run_mode`——托盘的一次点击不改变用户保存的运行方式（`main.rs:423-425`）；只有“退出服务”会发 `Request::Shutdown`、按需停止登录启动任务，并由前端确认后再退（`main.rs:325-332`、`main.rs:426-429`）。左键单击托盘图标唤回主窗口、右键弹出菜单；Linux 的 appindicator 不支持这个区分，按桌面环境原生行为处理（`main.rs:403-420`）。

轻量模式关掉窗口后留下的后台 `portal-cli daemon` 常驻内存约 3.7M；带 Tauri 与 WebView2 的界面进程约 80M，只在界面打开时存在。

账户保存方式有三种：系统凭证、配置文件明文、仅一次会话。默认使用系统凭证；配置文件明文只建议用于测试或没有可用系统凭据库的设备；仅一次会话只在 daemon 进程内存中使用密码，不写入配置文件或系统密钥库，daemon 退出、手动下线或清除会话后即释放。三者的落地位置与释放路径见[协议实现·第九节](./protocol-implementation.md#九凭据)。

保存方式与“掉线重拨”“后台服务”是彼此独立的设置：仅一次会话同样可以启用重连，重拨所需的凭据只在进程内保留，进程结束后自然消失；也允许写入后台服务，但 daemon 在登录系统时没有可读的密码，需要用户在 GUI 中登录一次。只有显式释放路径（手动下线、清除会话/Forget、停止后台服务、daemon 退出）会丢弃进程内凭据；关闭重连时也不会保留没有用途的重连凭据。自动重拨仍按“每 5 秒检测一次、连续 3 次缺失后触发、失败从 30 秒起指数退避、认证被拒则暂停”执行；这些时长含抖动，验收应依赖自动化测试验证状态转换，不要用固定的 15 秒作为手工验收期限。

## 后台服务

`service` 只安装**当前用户**的登录启动任务，永不安装 root 或系统级服务（`crates/portal-cli/src/service.rs:1`）。三平台都是启动 `portal-cli run --daemon`：

| 平台 | 位置与标识 | 说明 |
|---|---|---|
| macOS | `~/Library/LaunchAgents/net.dgcu.portal.plist`，`Label=net.dgcu.portal` | `RunAtLoad=true`、**没有** `KeepAlive`；任务已加载时 `load -w` 是空操作并且退出码仍为 0，所以判定成功还要看 stderr 里有没有 “Load failed”，`start` 走 `launchctl start <Label>`（`service.rs:41-132`） |
| Linux | `~/.config/systemd/user/dgcu-portal.service` | `After=`/`WantedBy=graphical-session.target`，`systemctl --user daemon-reload` + `enable`（`service.rs:133-158`） |
| Windows | 计划任务 `DGCU-Portal`，`schtasks /Create /F /SC ONLOGON /RL LIMITED` | 见下（`service.rs:175-347`） |

Windows 的 `ONLOGON` 触发器写在系统任务库里，普通权限调用 `schtasks` 一定被拒。创建与删除都先按当前权限试一次，被拒再通过 `powershell Start-Process -Verb RunAs` 提权重跑（提权创建的任务所有者是 Administrators，普通令牌里该组只用于拒绝，所以删除同样需要提权）；任务不存在时删除视为成功，便于反复开关；`/End` 不弹 UAC，退出 GUI 时静默结束。提权在 UAC 弹窗中选择“是”才会执行，取消会返回退出码 1223 并提示“未获得管理员授权”。

`enable` 之前会校验可执行文件确实存在，且路径不含换行与引号（`service.rs:26-33`）。OpenWrt 上 `service enable/start/stop` 一律拒绝，改用 procd：脚本位于 `packaging/openwrt/etc/init.d/portal-cli`，安装后使用

```bash
/etc/init.d/portal-cli enable
/etc/init.d/portal-cli start
/etc/init.d/portal-cli stop
```

OpenWrt 使用 `portal-cli run --daemon`，由 procd 负责拉起和停止；桌面系统服务也只启动 `portal-cli run --daemon`。

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
DGCU-Net-Portal.app
portal-cli
```

GUI 在运行时会沿自身可执行文件的祖先目录查找同包内的 `portal-cli`（`crates/portal-gui/src/main.rs:47-57`），因此打包必须让两者处于同一目录或父子目录关系。

版本号以 `Cargo.toml` 的 workspace `version` 为准，`crates/portal-gui/tauri.conf.json` 与 `crates/portal-gui/frontend/package.json` 需要同步；`scripts/package_macos.py` 与 `scripts/package_portable.py` 从 `tauri.conf.json` 读取版本，`.github/workflows/build.yml` 同样按它生成产物名并回填 `packaging/arch/PKGBUILD` 的 `pkgver`。
