# 校园网认证协议实现

> 对应版本：0.5.14（来源 `Cargo.toml` 的 workspace `version`，`tauri.conf.json` 与前端 `package.json` 同步）。
> 本文只描述代码里真实存在的行为，每条断言都给出 `文件:行`；协议事实与抓包依据见[校园网认证协议分析](./campus-network-auth.md)，运行、配置与构建见[开发者文档](./developer-guide.md)。

## 一、代码地图

工作区只有两个 package。协议相关代码全部在 `portal-cli` 的 library 里，`portal-gui` 通过本地 IPC 使用它。

| 文件 | 职责 | 主要符号 |
|---|---|---|
| `crates/portal-cli/src/lib.rs` | LFRadius 后台 HTTP 客户端、错误类型、URL 校验 | `PortalClient`、`AppError`、`OnlineSession`、`AccountInfo`、`validate_url`、`redact` |
| `crates/portal-cli/src/cmcc.rs` | CMCC Portal 入口构造与登录/代拨流程 | `CmccContext`、`PortalClient::cmcc_login_progress`、`Phase`、`PortalLoginOutcome` |
| `crates/portal-cli/src/discovery.rs` | 公共 HTTP 探测发现 Portal 入口 | `PortalClient::discover` |
| `crates/portal-cli/src/controller.rs` | 状态、会话绑定、掉线重拨、网络自愈 | `Controller`、`Snapshot` |
| `crates/portal-cli/src/daemon.rs` | IPC 服务端与两个轮询 worker | `daemon::serve`、`bind_endpoint` |
| `crates/portal-cli/src/ipc.rs` | 本地 socket 协议 | `SOCKET_NAME`、`Request`、`Response` |
| `crates/portal-cli/src/settings.rs` | 设置模型、持久化、凭据存取 | `Settings`、`UiPreferences`、`CredentialStore`、`ReconnectMode`、`RunMode`、`PollJitter` |
| `crates/portal-cli/src/service.rs` | 三平台用户级登录启动任务 | `enable`、`disable`、`start`、`stop` |
| `crates/portal-cli/src/network.rs` | 网卡枚举与解析 | `InterfaceInfo`、`NetworkContext`、`list`、`resolve` |
| `crates/portal-cli/src/traffic.rs` | 由后台计费计数推算速率 | `AccountingRates`、`Rate` |
| `crates/portal-cli/src/diagnose.rs` | 一次性连通性检测 | `Diagnostic`、`Reachability` |
| `crates/portal-cli/src/logging.rs` | 脱敏事件日志环形缓冲 | `LogBuffer`、`Event`、`LogEntry` |
| `crates/portal-cli/src/bin/portal-cli.rs` | CLI 入口，所有命令都转发给 daemon | `Cli`、`Command` |
| `crates/portal-cli/src/tests.rs` | 内置 Mock HTTP 服务端与协议回归测试 | `Mock` |
| `crates/portal-gui/src/main.rs` | Tauri 命令、托盘、daemon 拉起与版本握手 | 18 个 `#[tauri::command]` |
| `crates/portal-gui/src/update.rs` | GitHub Releases 版本比对（只检查，不下载安装） | `check`、`UpdateStatus` |

## 二、两条独立的 HTTP 会话

Portal 认证与后台自助是两套会话，代码里刻意分开（`lib.rs:1`）：`Controller::connect` 会构造两个 `PortalClient`，一个给后台（`api`），一个给 Portal（`portal`），各自持有独立的 Cookie jar（`controller.rs:187-198`）。

顺序是**先登录后台、再走 Portal**（`controller.rs:202-217`）：

1. 后台 `user_login` 成功后立刻读一次 `myinfo`（套餐与到期时间只在连接时读，它随订购变更而不随会话变化，`controller.rs:209-210`），再读一次会话列表作为 baseline。
2. baseline 里如果已经能唯一确定本机对应的会话，直接返回 `session_online`，跳过 Portal 登录（`controller.rs:218-234`）。
3. Portal 登录完成后，只有第一次后台登录失败时才补一次 `user_login`——Portal 的 Cookie 不能用来访问后台自助接口（`controller.rs:286-301`）。
4. Portal 成功但后台登录仍失败时，不报整体失败，而是置 `status="accepted"` 并提示重新登录后台查询会话（`controller.rs:292-298`）。

`PortalClient` 的 HTTP 行为是固定的：不自动跟随跳转（`redirect::Policy::none()`，这样 302 才能被显式判断）、请求超时 10 秒、连接超时 5 秒、TCP keepalive 30 秒、连接池空闲 15 秒、响应体上限 2 MiB、非 2xx 直接返回 `Http(状态码)`（`lib.rs:24-30`、`lib.rs:327-341`、`lib.rs:372-390`）。

## 三、Portal 入口从哪里来

`CmccContext` 只接受路径里含 `/portal.php/login/main/nasid/` 的可信入口 URL（`cmcc.rs:21-32`）。三条来源路径：

| 条件 | 做法 | 代码 |
|---|---|---|
| 用户粘贴了 Portal URL | 校验后按本机网卡上下文重写 | `controller.rs:267-277` |
| Portal URL 留空 | 按 DGCU 模板拼 `libs/portal/unify/portal.php/login/main/nasid/4/`，附加 `wlanacname=route1`、`vlan=0.0`、`iarmdst=captive.apple.com/hotspot-detect.html` | `controller.rs:238-248`、`cmcc.rs:80-96` |
| 模板表单不可用且 `probe_enabled` | 回退到公共 HTTP 探测发现入口 | `controller.rs:249-263`、`discovery.rs:72-104` |

模板路径优先是因为公共探测站点常常先超时（`controller.rs:240-242`）。判断“模板可用”用的是 `portal_form_available`：只 GET 入口并检查表单，不提交任何凭据（`cmcc.rs:176-180`）。

### 探测链

`discover` 的探测目标依次是配置的 `probe_url` 与两个备用地址 `http://www.msftconnecttest.com/redirect`、`http://connectivitycheck.gstatic.com/generate_204`（`discovery.rs:9-12`、`discovery.rs:72-81`）。约束：

- 单个探测 8 秒超时；超时记 `portal.probe.timeout`，换下一个（`discovery.rs:89-100`）。
- 跳转链最多 6 跳，出现重复 URL 判定为循环（`discovery.rs:107-116`、`discovery.rs:154`）。
- 认三种跳转：HTTP 3xx 的 `Location`、`Refresh` 响应头、页面里的 `meta http-equiv=refresh`（`discovery.rs:126-145`、`discovery.rs:20-35`）。
- 页面脚本只认**字面量**赋值与 `location.replace/assign` 调用；任何拼接、模板表达式或计算都需要浏览器，一律不跟随（`discovery.rs:36-51`）。从不执行页面 JS（`discovery.rs:1`）。
- 指向 Portal 入口的 `a[href]` 也可以跟随，但不跟随页面里的任意链接（`discovery.rs:58-67`）。
- 命中入口后必须属于配置的认证服务器，否则返回 `DiscoveryUntrusted` 并且不发送凭据（`discovery.rs:109-113`、`lib.rs:49-52`）。

### 本机身份注入

`with_network_context_options` 只重写 URL 上的 `wlanuserip`、`clientip`、`clientmac`、`paip`，其余参数原样保留（`cmcc.rs:41-71`）；提交表单时再用本机值覆盖 `usrip`、`usrmac`（`cmcc.rs:225-230`）。`paip` 取设置值（默认 `172.18.100.65`，`cmcc.rs:11`、`settings.rs:252-254`）；表单隐藏字段 `basip` 默认沿用服务端值，只有设置里填了才覆盖（`cmcc.rs:217-221`）。

网卡上下文来自 `network::resolve`：设置里留空时选第一个非 internal、同时具备 IPv4 与 MAC 的网卡（`network.rs:120-128`）。MAC 只认冒号分隔的 17 位写法，并排除全零与 `02:00:00:00:00:00`（`network.rs:34-47`）；库读不到 MAC 时，Linux 回退读 `/sys/class/net/<名称>/address`，macOS 回退 `ifconfig` 再回退 `networksetup`（`network.rs:49-88`）。

## 四、CMCC Portal 登录与代拨

入口是 `PortalClient::cmcc_login_progress`（`cmcc.rs:191-332`），流程与抓包一致：

| 步骤 | 行为 | 代码 |
|---|---|---|
| 1 | GET 入口，解析出含 `usrname` 的表单；要求 `method=post`、同名字段不得重复 | `cmcc.rs:199-202`、`cmcc.rs:119-158` |
| 2 | 表单 `action` 必须解析成 `.../login/cmcc_login/` 且在信任域内 | `cmcc.rs:203`、`cmcc.rs:333-339` |
| 3 | 校验 `nasid`/`usrmac`/`usrip`/`portal_version`/`portal_papchap` 非空，`basip` 有值或被覆盖；否则 `InvalidResponse("认证页缺少网络上下文")` | `cmcc.rs:206-221` |
| 4 | 只接受 `portal_version=1` + `portal_papchap=pap`，否则 `InvalidResponse("目前仅验证 Portal 1.0/PAP")` | `cmcc.rs:222-224` |
| 5 | 校验 `success`/`fail` 两个回调地址也在信任域内且后缀正确 | `cmcc.rs:231-242` |
| 6 | 填入 `usrname`/`passwd`/`treaty=on`，整张表单包在 `Zeroizing` 里编码后提交 | `cmcc.rs:243-258` |
| 7 | 从响应 HTML 读隐藏表单 `cmcc_login_value`，原样再 POST 一次 | `cmcc.rs:259-275` |
| 8 | 从 `xhr.send("l=...")` 取字面量载荷；正则不接受变量形式，取到后包 `Zeroizing` | `cmcc.rs:159-165`、`cmcc.rs:276` |
| 9 | 每 1 秒 POST `cmcc_login_result/`（带 `l`），直到正文严格等于 `success` | `cmcc.rs:279-284` |
| 10 | GET `login/success/`，按是否 3xx 分两条路 | `cmcc.rs:286-291` |

**普通上线分支**：响应不是跳转时，校验成功页 `.mbody` 文本含“登录成功”，返回 `PortalLoginOutcome::Direct`（`cmcc.rs:322-327`、`cmcc.rs:166-171`）。HTTP 200 本身不代表认证成功。

**代拨分支**：响应是跳转时——

1. `Location` 必须解析成 `.../login/__coa_search_page/` 且在信任域内，否则 `AppError::Redirect`（`cmcc.rs:291-297`）。
2. GET 等待页，正文必须同时包含 `[代拨]` 与 `__coa_search/`，否则 `InvalidResponse("代拨等待页")`；这次 GET 会建立轮询所需的 Cookie（`cmcc.rs:298-302`）。
3. 每 3 秒 POST `__coa_search/`，不带业务表单参数（`cmcc.rs:305-310`）。
4. 收到精确的 `success` 后 GET `login/success/success/1`，再次校验成功页，返回 `PortalLoginOutcome::Dialed`（`cmcc.rs:311-321`）。

轮询由同一个 `poll` 实现（`cmcc.rs:340-381`），Portal 与代拨共用：

- 截止时间是 20 秒；剩余时间不够再睡一个间隔时直接返回 `Timeout`，不会超出截止时间（`cmcc.rs:351-355`）。
- 单次请求本身再套一层剩余时间超时（`cmcc.rs:362-372`）。
- 判定条件是响应正文严格等于 `success`；其他非空正文继续等待，与抓包页面的行为一致（`cmcc.rs:373-379`）。
- 正文超过 8192 字节说明返回的是页面而不是状态，判为 `InvalidResponse("状态查询返回页面")`（`cmcc.rs:377-379`）。

代拨没有独立的适配器或 PPPOE 实现：它就是 Portal 登录里的一段 HTTP 轮询，是否进入由服务端响应决定。`PortalLoginOutcome::Dialed` 对应的界面文案是“Portal 与代拨均已确认成功”（`controller.rs:337-339`）。

### 信任域校验

所有从页面里读到的 URL（表单 `action`、回调地址、跳转 `Location`）都要过 `checked_action` → `trusted_url`：origin 必须等于配置服务器，path 必须以配置路径为前缀，并且解析结果必须正好等于预期的那个端点，任何一条不满足就返回 `AppError::Redirect`（`cmcc.rs:333-339`、`lib.rs:365-371`）。`Self::text` 也会在遇到跳转时直接报 `Redirect`，因为客户端禁用了自动跟随（`lib.rs:372-375`）。

### 登录进度

`Phase` 是登录期间的进度回调枚举，只有六个值：`Discovering`、`ReadingForm`、`Authenticating`、`WaitingPortal`、`WaitingDial`、`Accepted`（`cmcc.rs:104-113`）。它不是连接状态机，界面文案映射在 `crates/portal-gui/frontend/src/usePortal.ts:28`。

## 五、LFRadius 后台接口

后台响应统一是 `{v:{success,msg,url,wait_time}, d:...}` 信封，解析后 `v.success != 1` 一律返回 `AppError::Rejected`（`lib.rs:391-400`）。服务端把 JSON 标成 `text/html`，所以实现先按文本读再解析（`lib.rs:392-395`）。数字字段可能是字符串也可能是数字，`de_u64`/`de_ip` 负责兼容（`lib.rs:502-521`）。

| 接口 | 代码 | 说明 |
|---|---|---|
| `POST home.php?c=user&a=user_login` | `lib.rs:405-417` | 表单 `username`/`password`；成功后 Cookie 进入 jar |
| `GET home.php?c=user&a=myinfo` | `lib.rs:419-423` | 转成 `AccountInfo`；失败不影响会话查询（`controller.rs:210`、`controller.rs:303-305`） |
| `GET home.php?c=user&a=onlinelog&page=N&pagesize=15` | `lib.rs:424-442` | 逐页取到 `total` 为止，最多 100 页；`data` 为空但 `total` 未取满报 `InvalidResponse("分页不完整")` |
| `POST home.php?c=user&a=offline&r=person` | `lib.rs:451-474` | 九个表单字段，其中 `key` 复用 `radacctid`、`num=1` |

`AccountInfo` 只保留界面要用的四项，账号与凭据不进入该结构、也就不会随快照进前端（`lib.rs:164-176`）：套餐名取 `servername`；带宽档位从套餐名里第一个紧跟 `M`/`m`/`兆` 的数字解析成 `{n}Mbps`，刻意不维护档位白名单（`lib.rs:209-232`）；到期日取 `expiretime` 的日期部分（`lib.rs:242-248`）；套餐名含“学生”且认不出带宽档位时判定为未订购（`lib.rs:234-240`）。

下线确认是 `disconnect_and_confirm`：POST 下线表单后按 1 秒、2 秒、4 秒重查三次会话列表，目标 `radacctid` 消失才算成功，否则返回 `AppError::DisconnectPending`（`lib.rs:475-487`）。`disconnect_id` 会先刷新列表按 id 找到完整会话记录再下线，找不到报 `SessionNotFound`（`lib.rs:443-450`）——下线表单需要该记录的全部计费字段，不能只凭 id 拼。

## 六、状态、错误与日志

### Snapshot

`Snapshot` 是 daemon 回给 CLI 与 GUI 的唯一状态载体（`controller.rs:17-29`）：`status`、`message`、`sessions`、`rates`、`selected_id`、`background_paused`、`authenticated`、`one_session`、`account`。

`status` 是**字符串**而不是枚举（`controller.rs:19`）。全部取值、写入位置与界面标题（标题映射见 `crates/portal-gui/frontend/src/usePortal.ts:57`）：

| `status` | 界面标题 | 何时写入 |
|---|---|---|
| `idle` | 尚未连接 | 初始化 `controller.rs:85`；`forget` 清除本地会话 `controller.rs:113` |
| `authenticating` | 正在认证 | `connect` 开始 `controller.rs:201` |
| `accepted` | 认证成功 | Portal 已确认 `controller.rs:332`；Portal 成功但后台登录失败 `controller.rs:293` |
| `session_online` | 所选会话在线 | baseline 里已恢复本机会话 `controller.rs:225`；轮询确认所选会话仍在 `controller.rs:459` |
| `backend` | 已登录后台 | 只登录了后台、未选中会话 `controller.rs:334` |
| `offline` | 已下线 | 所选会话从列表消失 `controller.rs:452`；下线已确认 `controller.rs:530` |
| `error` | 连接失败 | `fail()` `controller.rs:129` |
| `unknown` | 下线待确认 | 下线请求失败或未确认 `controller.rs:519` |

### 错误类型

`AppError`（`lib.rs:32-71`）没有数字错误码，变体本身就是稳定标识，`#[error(...)]` 文案直接面向用户：

`InvalidUrl`、`Network(NetworkFault)`、`NetworkInterface(String)`、`DiscoveryTimeout`、`DiscoveryNotFound`、`DiscoveryUntrusted`、`DiscoveryLoop`、`Http(u16)`、`Rejected`、`InvalidResponse(&'static str)`、`Redirect`、`Timeout(&'static str)`、`SessionNotFound`、`DisconnectPending`、`AmbiguousSession`。

传输层失败先分类成 `NetworkFault::{Connect, Timeout, Other}`（`lib.rs:78-98`）：`Connect` 几乎总是本机绑定的源地址已失效（合盖唤醒换网段最典型），`Timeout` 更像链路慢或被拦，两者压成一句就没法从日志区分（`lib.rs:73-77`）。转换时只取 reqwest 自己的判定，不拼接 `source()` 链或系统错误原文，因为那可能带上本地地址与 URL（`lib.rs:85-97`）。

`Network` 的用户文案刻意点出两个最可能的原因——不在校园网环境、Portal 服务器离线，并有测试锁住这句不让改回含糊说法（`lib.rs:36-42`、`controller.rs:855-871`）。

### 日志

日志缓冲只接受类型化的 `Event`，不接字符串：没有请求体、URL、凭据、会话 ID 或原始服务端错误能进去（`logging.rs:1-2`）。每个 `Event` 映射到一组静态的 `(level, code, message)`（`logging.rs:74-197`），`LogEntry` 字段为 `sequence`、`timestamp_ms`、`level`、`code`、`message`（`logging.rs:199-206`）。缓冲是环形，上限 300 条（`logging.rs:11`、`logging.rs:254-257`）；关闭日志会清空历史（`logging.rs:217-230`）。

`AppError` → 日志事件的映射集中在 `logging.rs:259-277`。事件码清单（`logging.rs:74-197`）：

```text
log.enabled
backend.login            backend.accepted
portal.discover          portal.form          portal.submit
portal.wait              portal.poll          portal.accepted
portal.probe.fallback    portal.probe.http_redirect   portal.probe.html_redirect
portal.probe.found       portal.probe.no_redirect     portal.probe.timeout
portal.probe.untrusted   portal.probe.loop
dial.wait                dial.poll
sessions.read            account.read         session.select
session.disconnect       session.disconnected session.missing   session.pending
local.clear
reconnect.start          reconnect.paused
network.retry            network.rebind       network.rebind_failed
network.connect          network.error        network.interface
auth.rejected            auth.timeout         response.error
refresh.policy           poll.jitter
```

`redact()` 用于把账号一类值缩短成首尾各 2 字符，长度不超过 4 时全掩码（`lib.rs:522-532`）。

## 七、会话绑定与流量

### 绑定本机会话

两条互斥的规则，都拒绝“猜”：

- **新增会话绑定**：Portal 登录前记录 baseline 的 `radacctid` 集合（`controller.rs:323-328`），之后每次轮询比对；恰好一条新增就自动选中，两条以上则放弃自动绑定、要求用户手动选（`controller.rs:474-492`）。`new_session_id` 是同一语义的纯函数版本（`lib.rs:491-500`）。
- **本机 IP 匹配**：会话的 `framedipaddress` 等于所选网卡 IPv4，且这样的记录唯一，才自动恢复（`controller.rs:493-505`）。同一 IP 上有两条时不替用户挑——挑错等于把别人的设备下线或把流量算到自己头上（`controller.rs:825-854`）。

`select` 只接受当前列表里存在的 id，否则 `SessionNotFound`（`controller.rs:464-473`）。

### 速率

流量只来自后台计费计数，不读任何操作系统网卡统计（`traffic.rs:1`、`lib.rs:143`）。`AccountingRates::update`（`traffic.rs:20-53`）：

- 分母是 `acctsessiontime` 的差值，即 NAS 计费秒，**不是** 5 秒轮询的墙钟时间，否则会算出约 12 倍的虚高速率（`traffic.rs:32`）。
- 只有计费时间前进且两个计数都没有回退时才出速率（`traffic.rs:33`）；计数归零（NAS 换机器或重新计时）不能做减法，`u64` 会回绕成天文数字（`traffic.rs:71-81`）。
- 快照完全没变时返回 `None`，含义是“等待后台计费更新”而不是 0（`traffic.rs:42-49`）。
- 会话从列表消失就丢掉它的基线，同一 id 再出现时重新采样（`traffic.rs:21-22`）。
- `Rate` 的三个字段 `upload_bps`、`download_bps`、`sample_seconds` 都是 `Option`（`traffic.rs:6-11`）。

`traffic_enabled` 关闭时清空基线并返回空表，重新打开后从头采样（`controller.rs:440-447`）。

## 八、掉线检测、重拨与网络自愈

daemon 跑两个独立 worker（`daemon.rs:104-125`）：

- **刷新 worker**：间隔取 `refresh_policy.next_delay(poll_jitter)`，即 60 秒叠加抖动；策略为 `Disabled` 时返回 `None`，worker 按 1 秒空转（`daemon.rs:107-112`、`settings.rs:80-87`）。调用 `Controller::tick`，没有后台会话或刷新被禁用时直接返回（`controller.rs:534-545`）。
- **重拨 worker**：固定 5 秒叠加抖动，调用 `redial_tick`（`daemon.rs:116-125`）。

抖动按等级取 ±5%/±10%/±20%/0%，偏移量由当前时间的纳秒部分取模得到（`settings.rs:88-111`）。

### 掉线检测

`update_session_status` 只看所选会话是否还在列表里：不在就 `missing += 1` 并置 `offline`，在就 `missing` 归零并置 `session_online`（`controller.rs:448-463`）。重拨关闭时由 `refresh` 调用（`controller.rs:435-437`），重拨开启时由 `redial_tick` 调用（`controller.rs:566`）。

### 重拨

`redial_tick` 的前置条件是没有后台会话、重拨被禁用或已暂停时直接返回（`controller.rs:549-555`），每次轮询前先确认网卡绑定（`controller.rs:556-557`）。真正发起重拨要同时满足：未暂停、模式不是 `Disabled`、`missing >= 3`、`attempts < 3`、距上次尝试已超过退避窗口（`controller.rs:580-592`）。退避是 `30 秒 × 2^attempts` 叠加抖动（`controller.rs:423-428`）。

两种模式的差别（`controller.rs:598-621`）：

- `TerminateAndReconnect`：先 `clear()` 丢弃旧状态再连。
- `NewSession`：置 `reconnecting = true`，让 `connect` 跳过开头的 `clear()`（`controller.rs:153-158`），保留现有 API/Cookie 与会话行，直到新连接成功——重拨是事务性的。

结果处理（`controller.rs:622-655`）：

| 结果 | 行为 |
|---|---|
| 成功且绑定到会话 | 正常继续 |
| 成功但没绑定到会话 | `paused = true`，提示“自动重拨未完成或无法绑定新会话” |
| 瞬时错误 | 归还凭据、`attempts` 归零、回滚到重拨前的全部状态，等下一次掉线检测 |
| 其他错误（认证被拒） | `paused = true`，但**保留**进程内凭据供手动重试 |

错误分类分三档（`controller.rs:658-684`）：

- `transient_reconnect_error`：`Network(_)`、`Timeout(_)`、`DiscoveryTimeout`、`DiscoveryNotFound`、`Http(408|425|429|500..=599)`。
- `recoverable_poll_error`：上面这些再加 `Rejected`——合盖一夜后后台 Cookie 过期会让每次轮询都失败，而重连会用保存的凭据重新登录后台（`controller.rs:669-676`）。
- `retryable_connection_error`：`transient` 再加 `NetworkInterface`——网卡暂时消失是环境问题，不该像认证被拒那样停下来等人（`controller.rs:678-684`）。

### 网络自愈

轮询因网络或网卡失败时走 `retry_after_failure`：用当前网卡上下文重建连接并重连一次，事务性——只有 `connect` 成功才替换现有会话与客户端，失败时回滚（`controller.rs:694-756`）。它用**独立**的 `network_attempts` 计数，与会话掉线的 `attempts` 互不干扰（`controller.rs:45-46`），退避是 `30 秒 << min(network_attempts, 4)`，即 30 秒到 8 分钟封顶（`controller.rs:686-692`）。轮询成功一次就归零（`controller.rs:567`）。

网卡地址变化由 `rebind_changed_network` 处理：每次轮询重新解析网卡，上下文变了就用新的源地址重建 HTTP 客户端，Cookie 留在共享 jar 里，因此不需要重新认证（`controller.rs:355-385`、`lib.rs:274-288`、`lib.rs:342-350`）。解析失败时不动现有绑定，等下一次轮询再判断（`controller.rs:361-363`）。

`apply_settings` 让“保存设置”对已建立的连接立刻生效：后台地址变了调 `rebase`，网卡或代理变了清掉 `bound_network` 交给下次轮询重建；关闭重拨时立即丢弃进程内凭据（`controller.rs:386-422`）。

## 九、凭据

三种存储方式（`CredentialStore`，`settings.rs:50-57`）：

| 方式 | 落地位置 | 代码 |
|---|---|---|
| `System`（默认） | 系统凭据库，service `net.dgcu.portal`、user `saved-account` | `settings.rs:255-271` |
| `File` | `<配置目录>/credentials` 明文文件，unix 权限 `0600` | `settings.rs:258-260`、`settings.rs:272-295` |
| `Memory` | 只在 daemon 进程内存中 | `settings.rs:203-205` |

`Memory` 模式下 `normalize()` 会在保存前清空 `username`，所以账号和密码都不落盘（`settings.rs:200-205`）；`forget_password()` 同时删除明文文件与 keyring 条目，`NoEntry` 视为成功（`settings.rs:296-302`）。`Credential` 用 `Zeroize + ZeroizeOnDrop`，并且刻意不实现 `Debug`/`Serialize`（`lib.rs:105-115`）。

进程内凭据的保留与释放规则（与密码存在哪里无关）：

- 只要 `reconnect_mode != Disabled` 就保留在 `Controller` 内存里，重拨才有账号可用（`controller.rs:228-231`、`controller.rs:349-351`）。
- 关闭重拨后这份凭据没有后续用途，立即丢弃（`controller.rs:397-400`）。
- `Memory` + 重拨关闭时，任何失败或会话丢失都会整体 `clear()`（`controller.rs:116-128`、`controller.rs:454-456`）；`Memory` + 重拨开启时凭据必须留到下一次重拨（`controller.rs:116-119`）。
- 手动下线与 `forget` 是无条件释放路径（`controller.rs:506-509`、`controller.rs:110-115`）。
- 重拨暂停不等于释放，凭据留着供手动重试（`controller.rs:648-651`）。

daemon 启动时的自动登录只读 `System` 与 `File`，`Memory` 模式跳过（`daemon.rs:85-103`）。

## 十、IPC 与 daemon

端点名 `portal-cli.sock`（`ipc.rs:8`），用 `interprocess` 的 `GenericNamespaced` 绑定（`daemon.rs:34-38`）；Unix 上对应 `/tmp/portal-cli.sock`，测试直接按该路径验证（`tests.rs:550`、`tests.rs:538-544`）。协议是一行 JSON 请求换一行 JSON 响应（`ipc.rs:65-80`、`daemon.rs:142-155`）。

`Request`（serde `tag = "method"`、snake_case，`ipc.rs:10-33`）：

```text
status    shutdown  sessions  diagnose  logs  reload  clear_logs  forget
up { username, password, portal_url, backend_only }
down { session_id }
select { session_id }
```

`Response` 是单一结构而不是枚举：`ok`、`message`、`snapshot`、`logs`、`diagnostic`、`version`（`ipc.rs:35-45`）。`diagnostic` 只在 `diagnose` 时填充；`version` 由 daemon 在每个响应上统一打上，字段缺失说明对端是加版本号之前的老进程（`ipc.rs:41-44`、`daemon.rs:290-291`）。收到 `shutdown` 时先把响应写回，再延迟 80 毫秒退出进程（`daemon.rs:295-300`）。

`diagnose` 的处理刻意只取一份设置副本、不在探测期间持有控制器锁，否则数秒的探测会把界面每 2 秒一次的 `status` 轮询全堵在锁上（`daemon.rs:189-202`）。

### 端点独占与接管

`bind_endpoint` 解决“GUI 与 launchd 同时拉起 daemon 导致双开”的问题（`daemon.rs:24-63`）：先以 `try_overwrite(false)` 独占绑定，遇到 `AddrInUse` 时用 2 秒超时发一次 `status` 探活（`daemon.rs:22`、`daemon.rs:44-49`），再按结果分四种情况：

| 探活结果 | 行为 |
|---|---|
| 同版本实例在服务 | 让位，`serve` 打印提示后退出，绝不动对方端点（`daemon.rs:50-51`、`daemon.rs:75-78`） |
| 旧版本在服务 | 发 `shutdown`，等 300 毫秒后接管（`daemon.rs:52-57`） |
| 连得上但不回话 | 确实有进程占着端点，宁可等它也不抢（`daemon.rs:58-59`） |
| 连接被拒 | 判定为崩溃或退出留下的僵尸文件，删掉后 `try_overwrite(true)` 重绑（`daemon.rs:60-61`、`daemon.rs:65-71`） |

`try_overwrite(true)` 分不清“僵尸文件”和“别人正在服务的端点”，无条件开着就会静默删掉对方端点重绑，两个进程都活着但先启动的那个再也收不到请求（`daemon.rs:26-30`）。

### GUI 侧

GUI 的每个操作都经 IPC 转发给 daemon，不直接调库（`crates/portal-gui/src/main.rs:78-131`）；它链接 `portal-cli` library 只为了类型、`network::list()`、`service::*` 和拉起 daemon 二进制（`main.rs:2-9`、`main.rs:236-238`、`main.rs:281-284`）。请求前会做一次版本握手：端点上的 daemon 版本与自己不一致就请它退出，等 300 毫秒让端点释放（`main.rs:61-77`）——否则新界面发出的命令对旧进程是未知命令名，旧快照也缺新字段。请求失败时先请服务管理器拉起并等 5×200 毫秒，仍失败才 spawn 同包内的 `portal-cli run --daemon` 并等 25×200 毫秒（`main.rs:86-131`）；Windows 下加 `CREATE_NO_WINDOW` 避免闪黑框（`main.rs:106-111`）。daemon 二进制沿 GUI 可执行文件的祖先目录查找（`main.rs:47-57`）。

## 十一、连通性诊断

`diagnose::run` 只在用户手动触发时执行，永不读取或发送凭据（`diagnose.rs:1-5`）。结果 `Diagnostic` 有五个字段：`auth`、`auth_latency_ms`、`internet_direct`、`internet`、`internet_latency_ms`（`diagnose.rs:33-42`），单项结论是三档 `Reachability::{Reachable, Captive, Unreachable}`（`diagnose.rs:22-31`）。

- 认证探测：只要收到任何 HTTP 状态码就算 `Reachable`，只有连接层失败才是 `Unreachable`；超时 3 秒（`diagnose.rs:44-45`、`diagnose.rs:84-91`）。
- 外网探测跑两路并行：一路绑定所选网卡（与认证同源，代表校园网直连），一路跟随系统路由；超时 5 秒（`diagnose.rs:46-47`、`diagnose.rs:127-159`）。
- 汇总规则：直连有响应以直连为准；直连完全没响应但系统路径通了仍算可达，只是把直连那一档如实上报给界面（`diagnose.rs:108-125`）。
- 为什么要两路：本机开着 TUN 模式代理（Clash、Surge 之类）时 DNS 会被接管成 `198.18.0.0/15` 的假地址，只在那张虚拟网卡内可路由；把源地址钉在物理网卡上必然打不通，但用户上网恰恰是正常的——这是代理接管，不是校园网故障（`diagnose.rs:7-11`）。
- 判定外网响应：正文含 `Success` 为 `Reachable`；有跳转或正文以 `<` 开头为 `Captive`（认证页会拦截并重定向任何外网请求）；其余为 `Unreachable`（`diagnose.rs:49-61`、`diagnose.rs:93-106`）。
- 没有选定网卡时两路完全等价，省掉重复请求（`diagnose.rs:137-141`）。
- 三项探测并行，总耗时取决于最慢的一项而不是三者相加（`diagnose.rs:127`、`diagnose.rs:142-158`）。

## 十二、协议行为的验证方式

`crates/portal-cli/src/tests.rs` 内置一个跑在真实 `TcpListener` 上的最小 HTTP 服务端 `Mock`，按 mode 分支返回不同响应（`tests.rs:37-192`），协议路径的回归测试都建立在它上面：

| mode | 服务端行为 | 验证的协议点 |
|---|---|---|
| `direct` | `success/` 返回 200 成功页 | `PortalLoginOutcome::Direct`；隐藏 `basip` 用表单值而不是 URL 上的 `paip`；`cmcc_login_value` 被 URL 编码后原样透传；Portal Cookie 被带上（`tests.rs:193-214`） |
| `dial` | `success/` 302 到 `__coa_search_page/` | `Dialed`；轮询请求带上等待页设置的 Cookie 且不带表单体；最后请求 `success/success/1`（`tests.rs:215-234`） |
| `evil` | 302 指向外域的 `__coa_search_page/` | 返回 `AppError::Redirect`，不跟随（`tests.rs:235-245`） |
| `fake` | HTTP 200 但正文不是成功页 | 返回 `InvalidResponse`——200 不等于认证成功（`tests.rs:246-256`） |
| `timeout` | `__coa_search/` 一直返回空正文 | 返回 `Timeout("代拨")`，代拨等待有截止时间（`tests.rs:257-268`） |
| `discovery-http` / `-js` / `-meta` / `-loop` / `-foreign` / `-normal` | 探测链各分支 | 302、字面量 JS、meta refresh 都能发现入口；表达式拼接不跟随；外域被拒；循环被识别；普通页面不误判为在线（`tests.rs:96-121`、`discovery.rs:158-247`） |

后台接口侧，Mock 用 Cookie 区分登录态：未带后台 Cookie 时 `onlinelog` 与 `myinfo` 返回 `success:0`，因此能验证 `Rejected` 与 Cookie 跨命令存活（`tests.rs:154-168`、`tests.rs:269-288`）。`rebind` 后仍带着后台 Cookie 也有专门用例（`tests.rs:289-301`）。

日志脱敏有独立回归：断言账号、密码、Cookie 值都不会出现在日志 JSON 里（`tests.rs:284-287`、`logging.rs:322-333`）。

运行：

```bash
cargo test --workspace
```
