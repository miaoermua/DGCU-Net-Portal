# dgcu-portal

适用于 DGCU 的 LFRadius Portal 客户端，包含可复用 Rust 核心、CLI 和桌面 GUI。

## 运行

```bash
# 查询在线会话
cargo run -p portal-cli -- --server http://<认证服务器>/lfradius/ sessions

# 登录（不把密码写进 shell 历史）
cargo run -p portal-cli -- --server http://<认证服务器>/lfradius/ login --username <账号>

# 踢指定会话；程序会再次查询确认会话消失
cargo run -p portal-cli -- --server http://<认证服务器>/lfradius/ disconnect <radacctid>

# 启动桌面 GUI
cargo run -p portal-gui
```

GUI 与 CLI 共用 `portal-core`，目前实现 LFRadius 登录、在线会话查询和按 `radacctid` 踢线；代拨流程保留为“尚未采集”状态，不伪造成功。实现文档位于 `docs/`，这些本地分析文档已加入 `.gitignore`。

### 仅一次会话

CLI：

```bash
cargo run -p portal-cli -- --server http://<认证服务器>/lfradius/ login --username <账号> --one-session
```

程序会记录登录前的会话，登录后只选择本次新增会话；按回车确认下线，确认会话消失后清除本次账号和密码内存。若存在多个无法安全区分的旧会话，程序会拒绝自动踢线。GUI 的“仅一次会话”设置同样不会保存账号，且下线确认后清除账号、密码、会话和流量缓存。
