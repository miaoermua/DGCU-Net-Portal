# macOS 安装器磁盘映像

`.app` 之外的第二种分发形式：一个把 `.app` 和「应用程序」并排放好的拖拽安装窗口。

## 组成

- `background.svg` —— 安装器窗口的背景，16:9，网格 20 一格，两个图标的中心落在 (160,160) 和 (480,160) 的格线上；中间是箭头，下面一行提示文字用 Kalam。
- `render.swift` —— 用 AppKit 把 SVG 以 2x 渲染成 PNG，在 runner 上现画，不需要装任何东西；额外给出的字体只为这一次渲染注册，机器上不必安装。
- `installer.py` —— dmgbuild 的窗口描述，由它写出卷里的 `.DS_Store`，应用与「应用程序」的位置、图标大小都在这里定。

这一套做法来自 [canmi21/rdm](https://github.com/canmi21/rdm/tree/main/pkgs/macos)，只把应用名换成本项目的。

## 用法

```sh
cargo build --release -p portal-gui -p portal-cli --features portal-gui/custom-protocol --locked
python3 scripts/package_macos.py --output target/packages --dmg
```

加 `--dmg` 才出磁盘映像，不加就只有 `.app` 的 ZIP。卷名用 `--volume` 改，默认 `DGCU-Net-Portal`。

CI 里 macOS 的两个架构（arm64 / x86_64）都带 `--dmg`。产物分成两个 artifact：`macos-dmg-*` 进 Release，`macos-zip-*` 只作为 Actions 存档保留，不上传到 Release 页面。

## 依赖

dmgbuild 装在 `target/installer/venv` 自己的虚拟环境里，不会动系统的 Python。虚拟环境会被问「它还是不是一个虚拟环境」而不是「文件在不在」——CI 缓存整个 `target/`，上一轮留下的 venv 可能在新 runner 上被还原出来，而它的 python 已经不在 venv 里了，这时 pip 会以系统的身份运行并被拒绝，所以答案是「不是」就整个重建。

提示文字用的 Kalam 从 Google Fonts 现取，网络偶尔会把下载掐断，`curl` 带重试兜着（`--retry-all-errors` 是必需的，不然 SSL 握手中断不会重试）。

## 顺序

映像在 `codesign` 之后才做，里面装的是签好名的 `.app`。反过来做的话，卷里会是一个没签名的壳。
