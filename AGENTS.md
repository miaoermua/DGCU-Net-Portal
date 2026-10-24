# AGENTS.md

在仓库内工作的代理与开发者的操作约定。以下内容都对照当前代码（0.5.13）核实过；改代码前先看「工作区」「命令」，改文档前必读「文档保真」。

## 工作区

Cargo workspace 只有两个 package（`Cargo.toml` 的 `members`）：

| 位置 | 职责 |
| --- | --- |
| `crates/portal-cli` | 认证引擎、daemon、IPC、CLI、设置与凭据、脱敏日志、登录启动任务。认证/重试/代拨/会话逻辑**全部**在这里 |
| `crates/portal-gui` | Tauri 2 壳：窗口、托盘与菜单、18 个 `#[tauri::command]`、GitHub Releases 版本检查 |
| `crates/portal-gui/frontend` | Vue 3 + miuix-vue 界面，只经 Tauri command 与后端交互 |

GUI 不直接调用认证函数，而是把请求转给 `portal-cli run --daemon`（本地 socket `portal-cli.sock`，`crates/portal-cli/src/ipc.rs:8`）。文件级职责表只在 [docs/protocol-implementation.md](docs/protocol-implementation.md) 第一节维护，本文件不复制第二份。

## 命令

```bash
pnpm --dir crates/portal-gui/frontend install --frozen-lockfile
pnpm --dir crates/portal-gui/frontend test     # vitest
pnpm --dir crates/portal-gui/frontend build    # vue-tsc --noEmit && vite build

cargo test --workspace                          # 74 个 portal-cli + 6 个 portal-gui 测试
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check                      # 仓库无 rustfmt.toml，用默认风格

cargo build --release --locked -p portal-gui -p portal-cli --features portal-gui/custom-protocol
python3 scripts/package_macos.py --output target/packages --dmg
python3 scripts/package_portable.py --platform linux --arch x86_64 --output target/packages
```

- `.github/workflows/build.yml` 只在 `frontend` job 跑 `pnpm test`；Rust 侧只有 `cargo build`，**没有** `cargo test`/`clippy`/`fmt` 步骤，推送前必须本地跑一遍上面三条。
- 打包脚本不代编译，先 `cargo build --release` 再运行；`bundle.active` 为 `false`，正式产物由脚本产出，只有 AppImage 那一步用 `tauri build`。
- GUI 运行时沿自身可执行文件的祖先目录找同包内的 `portal-cli`，打包必须让两者处于同一目录或父子目录关系。
- 界面调试需要先产出 `crates/portal-gui/frontend/dist`（`tauri.conf.json` 的 `build.frontendDist`）。`--demo` 启动或可执行文件名是 `dgcu-portal-demo` 时进 Demo 模式，不联网、不写系统设置。

## 协议改动的测试落点

`crates/portal-cli/src/tests.rs` 里的 `Mock::new(mode)` 是本地 HTTP 假 Portal，模式有 `direct` / `fake` / `evil` / `timeout` / `discovery*` 等；`cmcc.rs`、`discovery.rs`、`controller.rs` 的回归测试都复用它。改协议解析、重定向信任或重拨时序时，加/改这一层的测试，不要只改文档。

## 版本号

三处手工镜像，没有自动同步脚本：

- 运行时：`Cargo.toml` 的 `[workspace.package].version`，代码用 `env!("CARGO_PKG_VERSION")` 给每个 IPC 响应打版本；GUI 据此判断端点上的 daemon 是否是需要替换的旧进程。
- 打包：`crates/portal-gui/tauri.conf.json` 的 `version`。CI 用 `jq -r .version` 生成 AppImage 名并回填 `packaging/arch/PKGBUILD` 的 `pkgver`，两个打包脚本也读它。`PKGBUILD` 里的 `pkgver=0.0.0` 是占位，不要手改。
- 界面：`crates/portal-gui/frontend/package.json` 的 `version`。

升版本一次改齐：`Cargo.toml`、`tauri.conf.json`、`frontend/package.json`、`README.md` 更新日志、三篇 docs 头部的「对应版本」。

## 安全边界

- 凭据：密码用 `Zeroizing` 包裹，持久化走系统钥匙串（服务名 `net.dgcu.portal`）或受限文件；不得写进设置 JSON、日志、panic 信息或 IPC 之外的通道。
- 日志：`crates/portal-cli/src/logging.rs:1` 的模块注释即约束——只有 `logging::Event` 类型事件进 buffer，没有 URL、请求体、凭据、会话 id、服务端原文。要加日志就新增事件码，不要给 buffer 传字符串。
- 服务任务：只装当前用户的登录启动任务（macOS LaunchAgent、Linux user systemd、Windows 计划任务 ONLOGON），不写 `/etc`、不装系统级服务、不请求 root。Windows 创建/删除任务需要 UAC，用户点"否"返回 1223。
- 抓包物料：`*.har` 已在 `.gitignore`，不要提交；HAR 是证据来源，不能原样当文档结论，先验证再写。

## 文档保真

1. 文档只写代码或抓包里已经存在的东西。关于实现的每句断言都要能给出 `文件:行`；给不出就删掉那句话，别写成"应该/计划/未来"。
2. 分析与实现分开。`docs/campus-network-auth.md` 只记抓包得到的协议事实（校园网侧变了才更新）；`docs/protocol-implementation.md` 记协议在代码里如何落地（跟着代码走）；`docs/developer-guide.md` 记运行、配置与构建。改动落在对应那篇，不要混写。
3. 设置键、命令名、错误码必须与代码一致。本仓库吃过亏的虚构物：`network_monitor`/`session_monitor`/`traffic_monitor` 这类不存在的设置键，以及 `AuthState`/`PortalAdapter`/`DialAdapter`/`ApiEnvelope` 这类不存在的类型名。
4. README 面向用户：只允许修正落后描述与去重，不擅自新增章节、不加功能宣传。

## 先问再动

- 代码里的过期版本串不属于文档范围，发现即汇报、等确认：`crates/portal-cli/src/discovery.rs:121` 的请求 UA 仍是 `DGCU-Portal/0.1.1`；`scripts/bundle_demo_macos.py:22-23` 的 demo `CFBundleVersion`/`CFBundleShortVersionString` 硬编码 `0.1.0`。
- 未获确认前不提交、不 push、不建 PR。工作区可能有他人的在途改动，任何 `git checkout`/`reset`/`clean` 之前先 `git status`。
- 本文件描述的是现状不是愿望：代码或流程变了，顺手把对应段落改准。
