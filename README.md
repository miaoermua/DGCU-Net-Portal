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



- **轻量**：底层使用 Rust + 界面使用 Tauri 2，兼顾体积小、后台运行占用低，主界面友好。不打开 GUI 时，只运行轻量的 `portal-cli` daemon 低至 3.7M RAM 占用。
- **跨平台**：支持 Windows、macOS、Linux，以及无桌面环境的 OpenWrt；桌面平台使用 `portal-gui`，服务和 OpenWrt 使用 `portal-cli`。
- **便捷**：配置简单，支持网卡自动识别、Portal/Unify 认证、掉线重拨、会话管理、流量统计。

另外额外做了兼容友校选项，理论上同套方案可用，但是具体未能够测试，需要友校人员参与测试反馈 PR。

## 安装及使用

通过本项目的 [Github Releases](https://github.com/miaoermua/DGCU-Net-Portal/releases) 上的快捷下载表，下载最新版本。如果访问不了 GitHub，可在下载链接前加上代理前缀（例如 `https://gh-proxy.com/` + 原始链接）加速下载。

### 简要安装方法

- Windows 用户下载 `-setup.exe` 双击安装，按照 installer 完成安装即可。
- Mac 用户下载 dmg，打开后把 DGCU-Net-Portal 拖入应用程序目录即可。
- Linux 选择对应发行版软件包进行安装即可。

### 设置项

各设置并无绝对过错，按需选用，即便全开也不会影响太多。


“登录项”：填写校园网登录账号密码

“写入用户后台服务（开机自启）”：系统启动后系统服务会自动唤起 portal-cli 进行认证，需要管理员授权。

“断线重拨”：在校园网失效后尝试断线重连，分为两个方法“新建后上线”和“断开后上线”，新建后上线可能会更快、而断开后上线需要程序完成断开校园网认证再重新上线。

“抖动”：用于降低特征，会同时控制“后台刷新”和“断线重连”的轮询时间



“管理会话”：是可用来查看设备踢下线/查看流量。

“后台流量统计” 和 “后台刷新” 为增强 GUI 功能，可以定期获取 portal 上下行流量数据和定期刷新会话数据。

“运行方式”决定：`常规` 与 `托盘启动` 关窗后收进托盘常驻，`轻量模式`（默认）关窗即结束界面与托盘，只留后台的 portal-cli daemon 继续认证，常驻内存约 3.7M。点托盘图标唤回主窗口，右键菜单里的“退出服务”才会彻底停止认证。

## 开发者

文档：

- [校园网认证协议分析](./docs/campus-network-auth.md)：抓包得到的协议事实与判断依据。
- [校园网认证协议实现](./docs/protocol-implementation.md)：协议在代码里如何落地。
- [开发者文档](./docs/developer-guide.md)：运行、配置与构建说明。
- [portal-cli 使用指南](./docs/portal-cli.md)：以 OpenWrt 为例的安装、配置与排障。
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

- **0.1.x** 完成协议认证
- **0.2.x** 完成客户端脚手架
- **0.3.x** 功能完整性优化
- **0.4.x** 修复使用问题
- **0.5.x** 异常处理，GUI 行为与视觉语言优化

## 声明

此工具为自由软件发布于 GPL 3.0 任何二改集成都需要开源，作者没有在其他平台要求你花钱购买此软件，谨防上当受骗。

此软件不构成“破解“校园网，因此使用该软件时你应该有你的学工账号且办理了校园网套餐确保你有可以登录的前提才能使用。

此软件由学生自主维护，暂时无与学校、校园网、运营商、服务商进行相关合作。校园网出现异常，无法连接可能并不是本工具问题，请先尝试手动连接或者询问校园内正常使用的小伙伴判断，如工具问题请携带日志和相关背景发起 issues。如校园网运营商服务商问题请联系他们，而不是提交给我。

本工具尽可能保持更新，如超出本人能力将谢绝解答或停止更新，校园网更新协议可能也导致该软件具有时效性，并非永久可用。
