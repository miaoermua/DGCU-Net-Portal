# portal-cli 使用指南（OpenWrt）

> 本文只写 OpenWrt 上把认证跑起来的操作路径：装、配、常用命令。完整命令参数与字段默认值见[开发者文档](./developer-guide.md#cli-命令)。

## 一、下载 Releases 安装

`portal-cli` 一个二进制同时是 CLI 和 daemon，认证只发生在 daemon 里。除 `config show`/`config set` 直接读写文件外，命令都经本地 socket `portal-cli.sock` 转发到 daemon（`crates/portal-cli/src/ipc.rs:8`），所以顺序永远是“先 daemon，后命令”。

### 1. 确认架构

```sh
uname -m                          # x86_64 / aarch64 / mips / mipsel / armv7l
cat /etc/openwrt_release          # DISTRIB_ARCH 更准
```

### 2. 下载对应资产

Releases 的 Linux 资产由 CI 在 Ubuntu 上原生构建，是 glibc 动态链接，只有 `x86_64` 与 `aarch64` 两种（`.github/workflows/build.yml:142-143`、`:165`）：

| 资产 | 架构 |
|---|---|
| `DGCU-Net-Portal-<ver>-linux-x86_64.tar.gz` | x86_64 |
| `DGCU-Net-Portal-<ver>-linux-aarch64.tar.gz` | aarch64 |

OpenWrt 固件是 musl，libc 与上表不一致时二进制会直接报 `not found`（找不到动态链接器）。资产列表里没有你的架构或 libc 就不要下其他包，跑不起来。

```sh
wget https://github.com/miaoermua/DGCU-Net-Portal/releases/download/v<ver>/DGCU-Net-Portal-<ver>-linux-x86_64.tar.gz
wget https://github.com/miaoermua/DGCU-Net-Portal/releases/download/v<ver>/SHA256SUMS.txt
sha256sum DGCU-Net-Portal-<ver>-linux-x86_64.tar.gz   # 与 SHA256SUMS.txt 里同名行对比
tar -xzf DGCU-Net-Portal-<ver>-linux-x86_64.tar.gz    # 解出 portal-cli / portal-gui / LICENSE
```

路由器上没有 `curl`/`wget` 时，在电脑上下载后 `scp` 过去即可。

### 3. 放进 `/usr/bin`

```sh
cp DGCU-Net-Portal-<ver>-linux-x86_64/portal-cli /usr/bin/
chmod 755 /usr/bin/portal-cli
portal-cli --version
```

服务脚本把二进制路径写死为 `/usr/bin/portal-cli`，放别处要同时改 `PROG`（`packaging/openwrt/etc/init.d/portal-cli:6`）。

## 二、安装服务

daemon 由 procd 托管，仓库自带脚本（`packaging/openwrt/etc/init.d/portal-cli`）：`run --daemon` 启动、异常退出后 5 秒重试（同文件 `:8-16`）。

### 1. 拷入脚本并设为开机自启

```sh
scp packaging/openwrt/etc/init.d/portal-cli root@router:/etc/init.d/
ssh root@router 'chmod +x /etc/init.d/portal-cli && /etc/init.d/portal-cli enable'
```

### 2. 启动并确认

```sh
ssh root@router '/etc/init.d/portal-cli start'
ssh root@router 'pidof portal-cli'          # 有 PID 说明 daemon 在跑
portal-cli status                           # 走 IPC，能通说明服务正常
logread -e portal-cli                       # daemon 日志
```

重复启动无害：已有实例时新进程打印“portal-cli daemon 已在运行，本进程退出”后正常返回（`crates/portal-cli/src/daemon.rs:215-219`）。停止与重启用脚本，不用 `service` 子命令：

```sh
/etc/init.d/portal-cli stop
/etc/init.d/portal-cli restart
```

**不要用 `portal-cli service enable/start/stop`**：OpenWrt 上会被拒绝并提示改用 init 脚本（`crates/portal-cli/src/service.rs:20-25`、`crates/portal-cli/src/service.rs:159-172`）。

## 三、配置

`settings.json` 与 `credentials` 在同一目录，由 `ProjectDirs` 按 `XDG_CONFIG_HOME`（其次 `HOME`）计算（`crates/portal-cli/src/settings.rs:166-170`、`crates/portal-cli/src/settings.rs:258-260`）。root 交互登录通常是 `/root/.config/portal/`，但 procd 拉起 daemon 时不保证 `HOME` 就是 `/root`，先确认：

```sh
tr '\000' '\n' < /proc/$(pidof portal-cli)/environ | grep ^HOME
```

如果 `HOME` 不是 `/root`，要么把配置放到 `$HOME/.config/portal/`，要么在 `/etc/init.d/portal-cli` 的 `start_service()` 里加一行 `procd_set_param env HOME=/root` 后 `/etc/init.d/portal-cli restart`。

### 方式 A：桌面 GUI 生成后拷过去

GUI 没有“导出配置”按钮，它的“保存设置”就是把整份 `Settings` 写进同一个 `settings.json`（`crates/portal-gui/src/main.rs:302-343`）：

1. 凭据存储选成**「配置文件（明文，仅测试）」**（`crates/portal-gui/frontend/src/App.vue:129`），否则密码进系统钥匙串，拷不走（`crates/portal-gui/src/main.rs:314-321`）。
2. 填账号、密码，保存。
3. 取 `settings.json` 与 `credentials`：macOS `~/Library/Application Support/net.dgcu.portal/`、Windows `%APPDATA%\dgcu\portal\config\`、Linux `~/.config/portal/`。
4. 拷到路由器上前面确认过的目录，`chmod 600`。

三个坑：目录名各平台不同、文件名相同；`service_enabled`/`run_mode` 在 OpenWrt 上无意义（daemon 由 procd 起），可改回 `false`；GUI 侧改动不会同步到路由器。

### 方式 B：在路由器上直接写

`Settings` 是 `#[serde(default)]`，没写的字段取默认值（`crates/portal-cli/src/settings.rs:114-116`）：

```sh
mkdir -p "$HOME/.config/portal"          # 换成前面确认过的目录
cat > "$HOME/.config/portal/settings.json" <<'EOF'
{
  "username": "你的学号",
  "credential_store": "file",
  "interface_name": "br-lan"
}
EOF
```

密码文件**用 `printf`，不要用 `echo`**：`file_password()` 原样读取，末尾换行会变成密码的一部分（`crates/portal-cli/src/settings.rs:290-295`）。

```sh
printf '%s' '你的密码' > "$HOME/.config/portal/credentials"
chmod 600 "$HOME/.config/portal/settings.json" "$HOME/.config/portal/credentials"
```

JSON 解析失败时整份配置会静默退回默认值（`crates/portal-cli/src/settings.rs:215-218`），改完必须复核：

```sh
portal-cli config show
portal-cli diagnose interfaces        # 名称、IPv4、MAC
portal-cli config set interface br-lan
```

`interface_name` 建议显式填写：留空时按网卡名排序取第一个“非虚拟 + 有 IPv4 + 有 MAC”的网卡（`crates/portal-cli/src/network.rs:157-175`），OpenWrt 上容易选到并非出口的接口。

### 开机自动认证的三个条件

daemon 启动时读一次配置，满足条件就用配置里的账号 + 凭据在后台认证（`crates/portal-cli/src/daemon.rs:225-243`）：

1. `username` 非空；
2. `credential_store` 是 `file`；
3. `credentials` 文件存在，内容是无换行的密码（建议 600，daemon 读取时不校验）。

任一条不满足，daemon 仍然会起、`status` 也正常，只是不会自动登录；这时用 `portal-cli up` 手动认证一次，可以区分是账号问题还是配置问题。改完配置要 `/etc/init.d/portal-cli restart`，daemon 只在启动时读一次。

## 四、常用命令

```sh
portal-cli status                     # 连接状态与会话数
portal-cli sessions                   # 会话 ID 与 IP
portal-cli up                         # 临时认证一次：交互输入账号密码，不落盘
portal-cli down <会话ID>              # 让指定会话下线
portal-cli logs                       # 读 daemon 的日志缓冲
portal-cli config show                # 打印整份配置
portal-cli diagnose interfaces        # 网卡清单，配置 interface 前先看
portal-cli config set interface br-lan
```

OpenWrt 日常开关服务用 init 脚本：

```sh
/etc/init.d/portal-cli start      # 启动
/etc/init.d/portal-cli stop       # 停止
/etc/init.d/portal-cli restart    # 重启
/etc/init.d/portal-cli enable     # 开机自启
```

`up` 适合排障：这次输入的凭据直接交给 daemon，不写配置文件，所以重启 daemon 后自动登录仍按配置里那套来。

## 五、非常用命令

```sh
portal-cli config set paip 172.18.100.65
portal-cli config set refresh 1m
portal-cli service status             # 走 IPC，其余 service 子命令在 OpenWrt 上会被拒绝
portal-cli run --daemon               # 前台跑 daemon 看实时输出；已有实例时新进程会直接退出
```

| 命令 | 说明 |
|---|---|
| `config set` | 只支持 9 个键：`interface`、`paip`、`basip`、`refresh`、`jitter`、`portal-probe`、`proxy`、`auto-redial`、`traffic`（`crates/portal-cli/src/bin/portal-cli.rs:94-149`）。账号、密码、`credential_store`、`server` 改不了，只能改 JSON 或用 GUI |
| `diagnose config` | 可用，打印生效配置（`crates/portal-cli/src/bin/portal-cli.rs:392-407`） |
| `diagnose backend/portal/probe/routes/request` | 未实现，报“该诊断命令尚未接入 daemon 请求链”（`crates/portal-cli/src/bin/portal-cli.rs:406`） |
| `config account` | 强制把 `credential_store` 写成 `system` 并写系统凭据库，路由器上没有 Secret Service，会失败（`crates/portal-cli/src/bin/portal-cli.rs:335-339`） |
| `config reset` | 报“配置重置需要明确指定配置文件路径”（`crates/portal-cli/src/bin/portal-cli.rs:358`），手工删 `settings.json` 更直接 |
| `logs --clear` | 报错，清理由 GUI 侧负责（`crates/portal-cli/src/bin/portal-cli.rs:315-318`） |
| `service enable/disable/start/stop/restart` | 拒绝并提示改用 `/etc/init.d/portal-cli`（`crates/portal-cli/src/service.rs:20-25`、`crates/portal-cli/src/service.rs:159-172`） |
| `run`（不带 `--daemon`） | 报“请使用 portal-cli run --daemon”（`crates/portal-cli/src/bin/portal-cli.rs:265`） |

## 六、排障速查

| 现象 | 原因 | 处理 |
|---|---|---|
| 命令报连接失败 | daemon 没起 | `/etc/init.d/portal-cli start`；`logread -e portal-cli` |
| `portal-cli` 直接报 `not found` | 资产与设备架构或 libc 不匹配 | 按安装一节核对 `uname -m` 与资产名 |
| `config show` 是默认值或不是你写的 | JSON 拼错被静默丢弃；或 CLI 与 daemon 的 `HOME` 不同 | 修正 JSON；按配置一节确认 daemon 的 `HOME` |
| 起来了但不自动登录 | `credential_store` 仍是 `system`，或密码带换行 | 改 `file`，用 `printf` 重写后 `/etc/init.d/portal-cli restart` |
| 认证失败但配置都对 | 网卡选错 | `diagnose interfaces` 后 `config set interface <名称>` |
| `config set` 报“未知配置项” | 该字段不在 9 个键之内 | 改 JSON，或从 GUI 生成 |
