# dgcu-portal

适用于 DGCU 的 LFRadius Portal 客户端，包含可复用 Rust 核心、CLI 和桌面 GUI。

## todo

- [ ] Windows 行为测试
- [ ] Linux 行为测试
- [ ] 优化代码结构，考虑分离
- [ ] 优化连接体验
- [ ] 刷新速度设置
- [ ] 兼容同方案友校

## 测试

MacOS 服务写入正常，托盘正常，认证过程正常。

## 运行

```bash
# 探测 Portal（只做 HTTP 探测，不执行认证）
cargo run -p portal-cli -- --server http://<认证服务器>/lfradius/ discover

# 列出本机网卡、当前 IPv4 和 MAC
cargo run -p portal-cli -- interfaces

# CMCC Portal 上线；默认先按 DGCU 模板提交，失败后才进行公共 HTTP 探测
cargo run -p portal-cli -- --server http://<认证服务器>/lfradius/ --username <账号> connect

# 禁用模板失败后的公共 HTTP 探测回退
cargo run -p portal-cli -- --server http://<认证服务器>/lfradius/ --no-probe --username <账号> connect

# 指定网卡上线；省略 --interface 时自动选择活动网卡
cargo run -p portal-cli -- --server http://<认证服务器>/lfradius/ --interface en0 --username <账号> connect

# 仅登录 LFRadius 自助后台，不等于校园网上线
cargo run -p portal-cli -- --server http://<认证服务器>/lfradius/ --username <账号> login

# 查询后台在线会话（流量来自 onlinelog 累计字段）
cargo run -p portal-cli -- --server http://<认证服务器>/lfradius/ --username <账号> sessions

# 踢指定会话；程序会再次查询确认会话消失
cargo run -p portal-cli -- --server http://<认证服务器>/lfradius/ disconnect <radacctid>

# 构建前端（Node.js >= 24；pnpm 11）
pnpm --dir crates/portal-gui/frontend install --frozen-lockfile
pnpm --dir crates/portal-gui/frontend build

# 启动桌面 GUI Demo（不连接校园网）
cargo run -p portal-gui -- --demo

# 启动桌面 GUI（内嵌上一步构建的页面）
cargo run -p portal-gui

# 仅调试前端：Vite 浏览器预览始终使用模拟数据
pnpm --dir crates/portal-gui/frontend dev
```

GUI 与 CLI 共用 `portal-core`。CMCC Portal 1.0/PAP 的普通登录和代拨分支已经接入：客户端默认按照 DGCU CMCC 模板构造 `main/nasid/4/` 入口，读取登录表单后透传服务端生成的隐藏载荷，处理成功页的 200/302，并在代拨页每 3 秒轮询 `__coa_search`，最长等待 20 秒。模板入口不可用时，默认再尝试公共 HTTP 探测；GUI 的“认证失败后自动探测”和 CLI 的 `--no-probe` 可以控制该回退。认证时只读取用户选择网卡的当前 IPv4 和 MAC，不读取网卡流量；在线流量只读取 LFRadius `onlinelog` 返回的累计字节。Portal URL 的 `wlanuserip`、`clientip`、`clientmac` 会用本机网卡值更新，`paip` 固定为 `172.18.100.65`；表单中的 `basip` 仍以 Portal 服务端返回值为准。`--demo` 只使用虚构数据。实现文档位于 `docs/`，这些本地分析文档已加入 `.gitignore`。

### 仅一次会话

CLI：

```bash
cargo run -p portal-cli -- --server http://<认证服务器>/lfradius/ --username <账号> connect --one-session
```

程序会在上线前记录后台会话，登录后只绑定唯一新增会话；存在多个新增会话时拒绝自动选择。按回车或 Ctrl-C 进入下线，确认目标会话消失后释放本次账号、密码、Cookie 和会话缓存。GUI 的“仅一次会话”设置同样禁止记住账号、自动重拨和后台服务。

## Tauri GUI 状态

GUI 使用 Tauri 2 + Vue 3 + [miuix-vue](https://github.com/YuKongA/miuix-vue)，并采用紧凑的顶部导航和偏好设置布局。`portal-core` 提供真实命令；`--demo` 用于查看界面和模拟后台计费更新。系统服务写入为当前用户级：macOS LaunchAgent、Linux systemd user service、Windows 登录任务。TUN/VPN 仍由操作系统决定；“绕过程序代理”只关闭 Rust HTTP 客户端的代理读取。

前端目录为 `crates/portal-gui/frontend/`，生成的 `dist/`、`node_modules/` 已忽略；依赖由 `pnpm-lock.yaml` 锁定。每次修改 Vue 页面后需重新执行 `pnpm build`，再构建 Rust 桌面程序。默认窗口 760×620，最小窗口 640×520，支持浅色、深色和跟随系统。

`miuix-vue@0.1.1` 的 npm 包把声明文件放在 `dist/src/index.d.ts`，但其导出声明路径指向 `dist/index.d.ts`；前端 `tsconfig.json` 仅为此增加类型路径映射，运行时仍使用原包。密码输入保留原生 `type=password`，因为该版本的 `MiuixInput` 仅支持文本类型。其余按钮、卡片、偏好开关、标签导航和消息条直接使用库组件。

前端校验：`pnpm --dir crates/portal-gui/frontend test`、`pnpm --dir crates/portal-gui/frontend build`。

## 界面、日志与程序图标

- 主界面不显示顶部名称栏；上下线合并为一个随状态切换的按钮，认证网站以旁边的图标打开。
- “管理会话”默认隐藏，可在设置中打开。隐藏时如需选择下线目标，会弹出选择框，不自动选择其他设备。
- 显示模式位于设置内，默认跟随系统；管理会话、显示模式和日志开关可以在已登录时即时保存。
- “仅一次会话”开启时隐藏“记住账号密码”，并在 Rust 中继续禁止保存凭据及后台自动认证。
- 日志默认关闭。打开后出现“查看 DGCU CLI 日志”入口，以弹窗展示当前 GUI 进程的共享 Rust 认证核心事件；不会额外启动 CLI，不读取其他进程的输出。CLI 可用 `--log` 将同源事件输出到 stderr。
- 日志只在内存保留最近 300 条，关闭即清空，不写日志文件；仅接受固定事件类型，不记录账号、密码、IP、MAC、URL、Cookie 或原始认证响应。
- 程序图标来自仓库根目录的 `xiaowei.png`。`scripts/make_icon.py`（需要 Pillow）生成 PNG、Windows ICO、macOS ICNS 和 macOS 模板托盘图标，原图不改动。Demo `.app` 也包含该图标。

## 构建可分发的 macOS 测试包

```bash
pnpm --dir crates/portal-gui/frontend install --frozen-lockfile
pnpm --dir crates/portal-gui/frontend build
cargo build --release -p portal-gui -p portal-cli --features portal-gui/custom-protocol --locked
python3 scripts/package_macos.py --output target/packages
```

输出包含真实客户端 `.app`、`dgcu-cli`、测试说明、ZIP 和 SHA-256。页面已内嵌，运行不依赖 Node 或 Rust，也不需要本地前端服务器。程序使用本地 ad-hoc 签名；未进行 Apple Developer ID 签名、公证或 Windows/Linux 实机验证。打包脚本不会自动安装应用或启用后台服务。

仓库：[miaoermua/dgcu-portal](https://github.com/miaoermua/dgcu-portal)。关于页只保留程序图标、版本与仓库入口。

### 0.2.1 探测与 Portal 模板兼容性修正

- 首选 HTTP 探测失败后，有限尝试 Windows / Android 常用探测地址，每个地址整个跳转链最长 8 秒。
- 支持 HTTP Location、Refresh 响应头、HTML meta refresh，以及字面量 `location.href` / `location.replace` / `location.assign`，不会执行远端脚本。
- 区分探测超时、没有认证跳转、服务器不匹配和跳转循环；日志仍不包含地址参数或认证载荷。
- 已经能上网时，网关可能不再返回认证页，此时应使用“管理会话 → 仅登录后台”查看状态。客户端不会把普通 HTTP 200 直接当作认证成功。
- 手工粘贴的完整 Portal URL 仍可直接使用，绕开公共探测站点的可达性问题。系统 TUN/VPN 路由不在本程序代理开关的控制范围内。
