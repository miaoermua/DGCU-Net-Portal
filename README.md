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

<img width="1744" height="1464" alt="portal" src="https://github.com/user-attachments/assets/3c4f84cf-e71d-4326-a035-c7f3740b4b81" />


- **轻量**：Rust + Tauri，体积小、运行占用低，主界面紧凑友好；不打开 GUI 时只运行轻量的 `portal-cli daemon`。
- **跨平台**：支持 Windows、macOS、Linux，以及无桌面环境的 OpenWrt；桌面平台使用 `portal-gui`，服务和 OpenWrt 使用 `portal-cli`。
- **便捷**：配置简单，支持网卡自动识别、Portal/Unify 认证、自动重试、掉线重拨、会话管理、日志和可选后台流量统计。

另外额外做了兼容友校选项，理论上同套方案可用，但是具体未能够测试，需要友校人员参与测试反馈 PR。

## 安装及使用

通过本项目的 Github Releases 里面下载最新的版本 zip 压缩包

- Mac 用户下载解压后将 DGCU-Net-Portal 拖入应用程序目录后即可完成安装。
- Windows 用户下载解压到你喜欢的应用安装目录下，将 portal-gui.exe 打开即可完成安装。

界面进程的去留由设置里的“运行方式”决定：`常规` 与 `托盘启动` 会把界面收进托盘常驻，随时点托盘图标唤回；`轻量模式`（默认）在关闭窗口时直接结束 GUI 与托盘，只留后台的 portal-cli daemon 继续认证，常驻内存约 3.7M。带 Tauri 与 WebView2 的界面进程约 80M，只在界面打开时存在。需要彻底停止认证时，用托盘菜单的“退出并停止后台服务”。

## 开发者

文档：

- [校园网认证协议与实现](./docs/campus-network-auth.md)：协议解析与代码实现。
- [开发者文档](./docs/developer-guide.md)：运行、配置与构建说明。
- [DeepWiki](https://deepwiki.com/miaoermua/DGCU-Net-Portal)

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

## 更新日志

- **0.1.x** 完成协议认证
- **0.2.x** 完成客户端脚手架
- **0.3.x** 功能完整性优化
- **0.4.x** 修复使用问题

## 声明

此工具为自由软件发布于 GPL 3.0 任何二改集成都需要开源，作者没有在其他平台要求你花钱购买此软件，谨防上当受骗。

此软件不构成“破解“校园网，因此使用该软件时你应该有你的学工账号且办理了校园网套餐确保你有可以登录的前提才能使用。

校园网出现异常，无法连接可能并不是本工具问题，请先尝试手动连接或者询问校园内正常使用的小伙伴判断，如工具问题请携带日志和相关背景发起 issues。如校园网运营商服务商问题请联系他们，而不是提交给我。

本工具尽可能保持更新，如超出本人能力将谢绝解答或停止更新，校园网更新协议可能也导致该软件具有时效性，并非永久可用。
