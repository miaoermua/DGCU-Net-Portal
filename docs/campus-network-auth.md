# 校园网认证协议分析

> 对应版本：0.5.14。
> 本文只记录抓包得到的协议事实与判断依据，不描述代码结构；这些协议在代码里如何落地见[校园网认证协议实现](./protocol-implementation.md)，运行、配置与构建见[开发者文档](./developer-guide.md)。

来源文件：

- `校园网.har`：LFRadius 用户后台登录、在线会话查询、按 `radacctid` 踢下线。
- `172.18.100.65.har`、`172.18.100.65（2）.har`：CMCC Portal 认证与代拨流程。

## 一、LFRadius 用户后台接口

### 结论

HAR 确实包含了：

1. 用户认证系统页面加载；
2. 用户登录接口成功；
3. 查询当前在线会话；
4. 对指定在线会话执行下线；
5. 下线后再次查询在线会话，在线会话数减少。

它**没有包含完整的“重新通过校园网 Portal 认证上线”过程**。下线之后只看到访问学校官网资源，没有发现新的 Portal 跳转、认证表单提交、认证成功回调、RADIUS Access-Request 或代拨接口请求。这份 HAR 里没有代拨请求；代拨的 HTTP 轮询出现在第二份 Portal 抓包中，见第二节。

### 请求时间线与接口含义

HAR 共 163 条请求，其中大量是前端静态 JS/CSS、图片和学校官网资源。与认证流程直接相关的请求如下：

| 顺序 | 方法 | 脱敏 URL | 结果 | 判断 |
|---:|---|---|---|---|
| 0 | GET | `/lfradius/web/admin/login` | 200 | 打开认证系统登录页 |
| 41 | POST | `/lfradius/home.php?c=user&a=user_login` | 200，JSON `v.success=1` | 用户名/密码登录成功 |
| 45–47 | GET | `/lfradius/home.php?c=user&a=header_info`、`address_and_phone`、`myinfo` | 200 | 加载用户资料、套餐、到期时间 |
| 93–96 | GET | `header_info`、`address_and_phone`、`person`、`realname_show` | 200 | 打开个人信息页，读取在线操作所需资料 |
| 97–105 | GET | `/lfradius/home.php?c=user&a=onlinelog&page=1&pagesize=15` | 200 | 轮询在线会话，开始时返回 2 条活动会话 |
| 106 | POST | `/lfradius/home.php?c=user&a=offline&r=person` | 200，JSON `v.success=1` | 对指定 `radacctid` 执行下线 |
| 107–109 | GET | `onlinelog?page=1&pagesize=15` | 200 | 下线后只剩 1 条活动会话 |
| 110 | GET | `https://学校官网/?rand=随机值` | 200 | 下线之后访问学校官网；没有看到 Portal 认证请求 |

静态资源和官网图片请求不属于认证协议，应从实现和测试夹具中排除。

### 登录接口

请求：

```http
POST /lfradius/home.php?c=user&a=user_login HTTP/1.1
Host: <认证服务器>
Content-Type: application/x-www-form-urlencoded
Origin: http://<认证服务器>
Referer: http://<认证服务器>/lfradius/web/admin/login

username=<账号>&password=<密码>
```

响应结构：

```json
{
  "v": {"success": 1, "msg": "", "url": "", "wait_time": 0},
  "d": []
}
```

观察：

- HAR 中没有记录 Cookie，也没有看到显式 `Authorization` 请求头。
- 这不代表一定没有会话：认证可能依赖浏览器未被 HAR 导出的存储、IP/网关上下文、服务端其他机制，或者 WebInspector 的导出策略没有保存相关字段。
- 不能只凭 HAR 复刻登录；需要用浏览器 DevTools 的“Copy as cURL”或在测试环境再次抓一条请求，确认 Cookie、Local Storage、CSRF、Referer 和实际认证凭证。
- 登录请求使用 HTTP 明文。生产客户端不能照搬，必须优先使用 HTTPS；如果学校设备只能提供 HTTP，应在产品中明确风险并限制到受信任的校园网地址。

### 在线会话查询

请求：

```http
GET /lfradius/home.php?c=user&a=onlinelog&page=1&pagesize=15 HTTP/1.1
```

响应中的会话字段包括：

```json
{
  "radacctid": "<会话记录 ID>",
  "username": "<账号>",
  "acctstarttime": "<开始时间>",
  "acctsessiontime": "<已用秒数>",
  "framedipaddress": "<用户 IP>",
  "acctinputoctets": "<入站字节数>",
  "acctoutputoctets": "<出站字节数>"
}
```

接口返回还带有：

```json
{"total": 2, "where": "where username='<账号>' and acctstoptime is null"}
```

实现含义：

- 当前“在线”是服务端 `radacct` 活动记录的视图，不是本机网卡是否有流量。
- `acctstoptime is null` 表明会话仍被系统认为活动。
- `radacctid` 是下线操作的关键标识，不能只用账号下线，否则多设备共享账号时可能踢错会话。
- 页面会重复轮询该接口；客户端可采用短轮询，但要增加退避、网络变化监听和最大轮询时长。

### 踢下线接口

请求：

```http
POST /lfradius/home.php?c=user&a=offline&r=person HTTP/1.1
Host: <认证服务器>
Content-Type: application/x-www-form-urlencoded
Origin: http://<认证服务器>
Referer: http://<认证服务器>/lfradius/web/home/person

radacctid=<会话记录 ID>&username=<账号>&acctstarttime=<开始时间>&acctsessiontime=<秒数>&framedipaddress=<用户 IP>&acctinputoctets=<入站字节数>&acctoutputoctets=<出站字节数>&key=<会话记录 ID>&num=1
```

响应：

```json
{"v":{"success":1,"msg":"","url":"","wait_time":0},"d":[]}
```

验证信号：

- 下线前 `onlinelog` 返回 2 条活动会话。
- 下线请求返回成功。
- 下线后 `onlinelog` 返回 1 条活动会话，且被指定的那条记录消失。

这说明后台接口至少触发了计费系统的会话下线/踢线逻辑。它没有证明“客户端已经从 NAS 断开”，除非能在 NAS、RADIUS Accounting 或实际网络流量侧进一步验证。

### “重新上线”目前缺失的证据

下线后的请求没有出现下列任何一种信号：

- Portal 302 跳转或登录页面；
- `/lfradius/libs/portal/...`、`portal.php`、`/nasid/...` 等 Portal URL；
- 认证提交字段，例如 `username`、`password`、`usrmac`、`usrip`、`source-ip` 的组合提交；
- CMCC Portal 1.0/2.0 的认证回调；
- RADIUS Access-Request/Access-Accept（HTTP HAR 本身也通常看不到 UDP RADIUS）；
- 认证成功后的页面或状态接口；
- 代拨/PPPOE 的启动、停止、重拨、回流或上报请求。

最后访问学校官网只能说明浏览器发起了 HTTPS 请求，不能证明该账号完成了重新认证。可能的解释包括：

1. 设备被踢后网络尚未真正断开，旧会话或另一条在线会话仍然可用；
2. NAS 的踢线/COA 是异步的，HAR 在 Portal 弹出前结束；
3. 浏览器没有重新发起被劫持的 HTTP 探测请求；
4. 学校使用的是系统级代拨或 PPPOE，浏览器 HAR 看不到代拨过程；
5. 代拨窗口没有完成，或者代拨配置尚未生效。

## 二、CMCC Portal 认证与代拨

### 结论

两份文件不是两个完全独立的最小流程：第二份包含第一份账号的 Portal 片段，随后还包含后台登录、踢线和第二个账号的再次 Portal 登录。

两个账号的 Portal 行为不同：

| 流程 | 结果 |
|---|---|
| 第一个账号 | CMCC Portal 认证成功后直接返回 `/login/success/`，没有代拨轮询 |
| 第二个账号 | CMCC Portal 认证成功后，`/login/success/` 返回 302 到 `__coa_search_page/`；页面显示 `[代拨]`，轮询 `__coa_search/` 返回 `success` 后才进入最终成功页 |

因此代拨是“认证成功后的异步等待”，是否进入代拨分支由服务端/NAS 状态决定。客户端不应该根据账号、套餐名或固定等待时间猜测，而应该跟随 HTTP 响应判断。

### 第一个账号：普通 Portal 流程

第一份 HAR 共 7 条请求，核心流程如下：

```text
POST /lfradius/libs/portal/unify/portal.php/login/cmcc_login/
  usrname, passwd, treaty=on
  nasid, usrmac, usrip, basip
  success, fail, offline=1
  portal_version=1, portal_papchap=pap
       ↓
POST /lfradius/libs/portal/unify/portal.php/login/cmcc_login/
  cmcc_login_value=<服务端生成的隐藏值>
       ↓
POST /lfradius/libs/portal/unify/portal.php/login/cmcc_login_result/
  l=<服务端生成的隐藏值>
  响应：success
       ↓
GET /lfradius/libs/portal/unify/portal.php/login/success/
  HTTP 200
  页面显示：登录成功
```

第一份 HAR 的时间从开始提交到成功页约 1.3 秒。第二次 POST 不是客户端重新构造的业务参数，而是第一步响应返回 HTML 中的隐藏表单自动提交：

```html
<form method="post" action=".../cmcc_login/">
  <input name="cmcc_login_value" type="hidden" value="..." />
</form>
<script>document.forms['login'].submit()</script>
```

`cmcc_login_result` 的 `l` 字段同样是服务端生成的状态载荷。客户端应把它当作不透明值透传，不能自行生成或把解码后的内容写入日志。

### 第二个账号：含代拨流程

第二份 HAR 后半段从一次系统探测开始：

```http
GET /lfradius/libs/portal/unify/portal.php/login/main/nasid/4/
    ?wlanuserip=<用户IP>
    &clientip=<用户IP>
    &wlanacname=route1
    &clientmac=<用户MAC>
    &paip=<NAS/Portal地址>
    &vlan=0.0
    &iarmdst=captive.apple.com/hotspot-detect.html
```

这几个字段的含义可以按当前设备解释为：

| 字段 | 作用 |
|---|---|
| `nasid/4` | LFRadius 中配置的 NAS ID |
| `wlanuserip` / `clientip` | 终端在校园网中的 IP |
| `wlanacname` | 接入设备标识，抓包中为 `route1` |
| `clientmac` | 终端 MAC |
| `paip` | 接入设备或 Portal 侧地址 |
| `vlan` | VLAN 上报值 |
| `iarmdst` | 被劫持的探测目标，抓包中是 Apple Captive Network Assistant 地址 |

随后提交的表单和普通流程相同，仍然是 Portal 1.0 + PAP：

```text
POST /login/cmcc_login/
POST /login/cmcc_login/              # cmcc_login_value 自动提交
POST /login/cmcc_login_result/       # l 不透明载荷，返回 success
```

差异出现在下一步：

```http
GET /lfradius/libs/portal/unify/portal.php/login/success/
HTTP/1.1 302 Found
Location: /lfradius/libs/portal/unify/portal.php/login/__coa_search_page/
```

`__coa_search_page/` 返回一个等待页面，正文明确包含 `[代拨]`。页面内 JavaScript 的逻辑是：

```text
初始倒计时约 20 秒
每 3 秒 POST /login/__coa_search/
不带业务表单参数
响应正文严格等于 success 时停止轮询
跳转 /login/success/success/1
其他非空响应显示为状态文本
```

抓包中的实际代拨轮询：

```http
GET  /lfradius/libs/portal/unify/portal.php/login/__coa_search_page/
POST /lfradius/libs/portal/unify/portal.php/login/__coa_search/
     响应正文：success
GET  /lfradius/libs/portal/unify/portal.php/login/success/success/1
     页面显示：登录成功
```

当前抓包只记录到一次 `__coa_search`，约 3 秒后返回 `success`。因此不能把 3 秒硬编码为代拨完成时间；正确实现是最多轮询 20 秒，收到精确的 `success` 立即完成，超时返回代拨超时。

### 两个账号的后台会话背景

第二份 HAR 在第二次 Portal 登录之前还操作了 LFRadius 用户后台：

1. 后台用户登录成功。
2. `onlinelog` 返回两个活动会话。
3. 对其中一条 `radacctid` 调用 `POST /home.php?c=user&a=offline&r=person`。
4. 再次查询后目标会话消失，另一条旧会话仍存在。
5. 随后发生新的 Portal 探测和第二个账号登录。

这说明测试环境中可能同时存在旧会话。实现“仅一次会话”时，不能按用户名直接踢线，必须记录登录前的会话集合，登录后只选择新增的 `radacctid`；如果不能唯一确定，就拒绝自动踢线。

### 协议本身对客户端的约束

以下结论来自抓包，与具体实现无关：

- `cmcc_login_value` 与 `l` 都是服务端生成的不透明载荷，只能原样透传；客户端不得自行构造、解码后使用或写入日志。
- 是否进入代拨分支只能由 `/login/success/` 的响应决定：HTTP 200 且成功页即普通上线，302 到 `__coa_search_page/` 才进入代拨轮询。不能按账号、套餐名或固定等待时间猜测。
- `__coa_search/` 的判定条件是响应正文严格等于 `success`；其他非空正文在抓包页面里只显示为状态文本，不代表失败，因此只能等到 20 秒倒计时结束才算超时。
- 代拨轮询请求不带业务表单参数，但依赖 `__coa_search_page/` 那次 GET 建立的会话状态（Cookie）；跳过该 GET 直接轮询没有抓包依据。
- Portal 表单字段里的 `nasid`、`usrmac`、`usrip`、`basip`、`success`、`fail`、`portal_version`、`portal_papchap` 都来自认证页返回的 HTML，客户端替换的只有本机身份相关的值；`paip` 是 URL 上的 Portal 侧地址，与表单隐藏字段 `basip` 不是同一个东西，不能互相代入。
- 抓包中的 Portal 版本与认证方式固定为 `portal_version=1`、`portal_papchap=pap`；Portal 2.0 与 CHAP 没有采集样本。
- 后台“在线”是服务端计费记录的视图，踢线成功不等于 NAS 已经断开，必须再次查询会话列表确认。

## 三、重要安全事项

原始 HAR 中包含明文密码和个人信息，登录请求使用 HTTP。应立即修改该测试账号的密码，把原始 HAR 移出共享目录并不提交 Git，后续只使用脱敏副本。

两份 HAR 的 Portal 表单同样是 HTTP，且 Portal 页面会把账号、密码和设备网络参数放入 HTML、隐藏字段或短期载荷。生产客户端应：

- 优先使用 HTTPS；若校园网只能提供 HTTP，要把风险明确显示给用户。
- 不在 URL、日志、崩溃报告或配置文件中保存密码和 opaque token。
- 不把 `cmcc_login_value` 或 `l` 解码后打印出来。
- 只在内存中保存本次会话；启用“仅一次会话”时，下线确认后清除账号、密码、临时载荷和会话缓存。
- 对 `nasid`、Portal 主机和原始跳转地址做白名单校验，禁止前端拼接任意内网请求。

## 四、抓包未覆盖的部分

以下内容在现有 HAR 中没有样本，不能从这份材料推断：

- UDP 侧的 RADIUS Access-Request/Access-Accept 与 Accounting（HTTP HAR 本身也看不到）。
- 代拨的启动、停止、重拨与凭据下发；抓包只覆盖到代拨状态的 HTTP 轮询。
- PPPOE 或系统级代拨客户端的任何请求。
- 后台登录是否还依赖 Cookie 之外的凭证（HAR 未导出 Cookie/Local Storage/CSRF）。
- Portal 2.0 与 CHAP：抓包里只有 `portal_version=1`、`portal_papchap=pap`。
