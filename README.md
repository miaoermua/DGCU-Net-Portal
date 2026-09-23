# DGCU-Net-Portal

## 特色功能

- **轻量**：Rust engine + Tauri GUI，体积小、运行占用低，主界面紧凑友好；不打开 GUI 时只运行轻量的 `portal-cli daemon`。
- **跨平台**：支持 Windows、macOS、Linux，以及无桌面环境的 OpenWrt；桌面平台使用 `portal-gui`，服务和 OpenWrt 使用 `portal-cli`。
- **便捷**：配置简单，支持网卡自动识别、Portal/Unify 认证、自动重试、掉线重拨、会话管理、日志和可选后台流量统计。

文档：

- [校园网认证协议与实现](./docs/campus-network-auth.md)：协议解析与代码实现。
- [开发者文档](./docs/developer-guide.md)：运行、配置与构建说明。

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

## 更新日志

- **0.1.x** 完成协议认证
- **0.2.x** 完成客户端脚手架
- **0.3.x** 功能完整性优化
