<div align="center">
  <img src="./docs/xiaowei.png" width="100" height="100" alt="DGCU-Net-Portal 图标" />
  <h1>DGCU-Net-Portal</h1>
  <p>DGCU-Net-Portal 是适用于东莞城市学院的校园网登录助手客户端，在 lfradius portal 下完成自动化登录解放双手。</p>

  <p>
    <a href="https://github.com/miaoermua/DGCU-Net-Portal"><img src="https://m3-markdown-badges.vercel.app/stars/3/3/miaoermua/DGCU-Net-Portal" alt="Stars" /></a>
    <a href="https://github.com/miaoermua/DGCU-Net-Portal/issues"><img src="https://m3-markdown-badges.vercel.app/issues/1/2/miaoermua/DGCU-Net-Portal" alt="Issues" /></a>
    <a href="https://www.miaoer.net/sponsor"><img src="https://ziadoua.github.io/m3-Markdown-Badges/badges/Sponsor/sponsor1.svg" alt="Support" /></a>
    <a href="https://github.com/miaoermua/DGCU-Net-Portal/blob/main/LICENSE"><img src="https://ziadoua.github.io/m3-Markdown-Badges/badges/LicenceGPLv3/licencegplv31.svg" alt="COPYING" /></a>
  </p>

  <p>
    <img src="https://ziadoua.github.io/m3-Markdown-Badges/badges/Rust/rust1.svg" alt="Rust" />
    <img src="https://ziadoua.github.io/m3-Markdown-Badges/badges/ViteJS/vitejs1.svg" alt="ViteJS" />
    <img src="https://ziadoua.github.io/m3-Markdown-Badges/badges/Vue/vue1.svg" alt="Vue" />
    <img src="https://ziadoua.github.io/m3-Markdown-Badges/badges/TypeScript/typescript1.svg" alt="TypeScript" />
  </p>
</div>

## 功能

<img width="1744" height="1464" alt="portal" src="https://github.com/user-attachments/assets/05dd0e71-d6f2-4bec-9783-6b66db2fda3c" />



- **轻量**：Rust + Tauri，体积小、运行占用低，主界面紧凑友好；不打开 GUI 时只运行轻量的 `portal-cli daemon`。
- **跨平台**：支持 Windows、macOS、Linux，以及无桌面环境的 OpenWrt；桌面平台使用 `portal-gui`，服务和 OpenWrt 使用 `portal-cli`。
- **便捷**：配置简单，支持网卡自动识别、Portal/Unify 认证、自动重试、掉线重拨、会话管理、日志和可选后台流量统计。

另外额外做了兼容友校选项，理论上同套方案可用，但是具体未能够测试，需要友校人员参与测试反馈 PR。

## 安装及使用

通过本项目的 Github Releases 下载最新版本

- Windows 用户下载 `-setup.exe` 双击安装，全程不需要管理员权限，装在当前用户目录下；装完在开始菜单里启动，在「设置 → 应用」里可卸载。也可以下载 zip 解压到任意目录，直接运行 `portal-gui.exe`。
- Mac 用户下载 dmg，打开后把 DGCU-Net-Portal 拖入应用程序目录即可。

界面进程的去留由设置里的“运行方式”决定：`常规` 与 `托盘启动` 关窗后收进托盘常驻，`轻量模式`（默认）关窗即结束界面与托盘，只留后台的 portal-cli daemon 继续认证，常驻内存约 3.7M。点托盘图标唤回主窗口，右键菜单里的“退出服务”才会彻底停止认证；托盘菜单项、各运行方式的启动与关窗行为等细节见[开发者文档](./docs/developer-guide.md)。

## 开发者

文档：

- [校园网认证协议分析](./docs/campus-network-auth.md)：抓包得到的协议事实与判断依据。
- [校园网认证协议实现](./docs/protocol-implementation.md)：协议在代码里如何落地。
- [开发者文档](./docs/developer-guide.md)：运行、配置与构建说明。
- [DeepWiki](https://deepwiki.com/miaoermua/DGCU-Net-Portal)

```text
crates/portal-cli/src/
├── lib.rs          # engine library
├── controller.rs   # 状态、重试和重拨
├── cmcc.rs         # CMCC/Unify Portal
├── discovery.rs    # 公共 HTTP 探测发现 Portal 入口
├── network.rs      # 网卡 IPv4/MAC 和本地网络上下文
├── traffic.rs      # 由后台计费计数推算速率
├── diagnose.rs     # 连通性检测
├── settings.rs     # 设置持久化与凭据存取
├── service.rs      # 三平台登录启动任务
├── logging.rs      # 脱敏日志
├── daemon.rs       # daemon IPC 服务
├── ipc.rs          # IPC 请求/响应
├── tests.rs        # Mock 服务端与协议回归测试
└── bin/portal-cli.rs

crates/portal-gui/
├── src/main.rs     # Tauri IPC 客户端和桌面服务控制
├── src/update.rs   # GitHub Releases 版本检查
└── frontend/       # Vue + miuix-vue
```

## 更新日志

- **0.5.16** 版本同步，更新工作区版本号与文档对应版本说明。
- **0.1.x** 完成协议认证
- **0.2.x** 完成客户端脚手架
- **0.3.x** 功能完整性优化
- **0.4.x** 修复使用问题
- **0.5.x** 掉线自愈、后台服务与更新检查，跨区域处理与视觉语言优化

## 声明

此工具为自由软件发布于 GPL 3.0 任何二改集成都需要开源，作者没有在其他平台要求你花钱购买此软件，谨防上当受骗。

此软件不构成“破解“校园网，因此使用该软件时你应该有你的学工账号且办理了校园网套餐确保你有可以登录的前提才能使用。

此软件由学生自主维护，暂时无与学校、校园网、运营商、服务商进行相关合作。校园网出现异常，无法连接可能并不是本工具问题，请先尝试手动连接或者询问校园内正常使用的小伙伴判断，如工具问题请携带日志和相关背景发起 issues。如校园网运营商服务商问题请联系他们，而不是提交给我。

本工具尽可能保持更新，如超出本人能力将谢绝解答或停止更新，校园网更新协议可能也导致该软件具有时效性，并非永久可用。

