# dgcu-portal

适用于 DGCU 的 LFRadius Portal 客户端，包含可复用 Rust 核心、CLI 和桌面 GUI。

## 运行

```bash
# 探测 Portal（只做 HTTP 探测，不读取网卡）
cargo run -p portal-cli -- --server http://<认证服务器>/lfradius/ discover

# CMCC Portal 上线；密码使用隐藏输入
cargo run -p portal-cli -- --server http://<认证服务器>/lfradius/ --username <账号> connect

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

GUI 与 CLI 共用 `portal-core`。CMCC Portal 1.0/PAP 的普通登录和代拨分支已经接入：客户端解析登录表单，透传服务端生成的隐藏载荷，处理成功页的 200/302，并在代拨页每 3 秒轮询 `__coa_search`，最长等待 20 秒。在线流量只读取 LFRadius `onlinelog` 返回的累计字节，不采集本机网卡。`--demo` 只使用虚构数据。实现文档位于 `docs/`，这些本地分析文档已加入 `.gitignore`。

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
