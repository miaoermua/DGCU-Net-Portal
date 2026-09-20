"""Package release binaries into a local ad-hoc signed macOS test application.

No installation or service registration is performed. Build first with:
cargo build --release -p portal-gui -p portal-cli --features portal-gui/custom-protocol --locked
"""
import argparse
import hashlib
import json
import platform
import plistlib
import shutil
import subprocess
from pathlib import Path

parser = argparse.ArgumentParser()
parser.add_argument('--output', type=Path, required=True)
args = parser.parse_args()
root = Path(__file__).resolve().parents[1]
config = json.loads((root / 'crates/portal-gui/tauri.conf.json').read_text())
version = config['version']
arch = platform.machine()
name = f'DGCU-Portal-{version}-macos-{arch}'
folder = args.output.resolve() / name
if folder.exists():
    raise SystemExit(f'Refusing to overwrite existing package: {folder}')
binary = root / 'target/release/portal-gui'
cli = root / 'target/release/portal-cli'
for file in [binary, cli]:
    subprocess.run(['file', str(file)], check=True)
app = folder / 'DGCU Portal.app'
macos = app / 'Contents/MacOS'
resources = app / 'Contents/Resources'
macos.mkdir(parents=True)
resources.mkdir(parents=True)
shutil.copy2(binary, macos / 'dgcu-portal')
shutil.copy2(cli, folder / 'dgcu-cli')
shutil.copy2(root / 'crates/portal-gui/icons/icon.icns', resources / 'icon.icns')
shutil.copy2(root / 'LICENSE', folder / 'LICENSE')
with (app / 'Contents/Info.plist').open('wb') as stream:
    plistlib.dump({
        'CFBundleExecutable': 'dgcu-portal',
        'CFBundleIdentifier': config['identifier'],
        'CFBundleName': config['productName'],
        'CFBundleDisplayName': config['productName'],
        'CFBundlePackageType': 'APPL',
        'CFBundleIconFile': 'icon.icns',
        'CFBundleVersion': version,
        'CFBundleShortVersionString': version,
        'LSMinimumSystemVersion': '12.0',
        'NSHighResolutionCapable': True,
        'NSAppTransportSecurity': {'NSAllowsLocalNetworking': True},
    }, stream)
subprocess.run(['codesign', '--force', '--sign', '-', str(app)], check=True)
subprocess.run(['codesign', '--verify', '--deep', '--strict', str(app)], check=True)
subprocess.run(['codesign', '--force', '--sign', '-', str(folder / 'dgcu-cli')], check=True)
guide = f'''# DGCU Portal {version} · macOS {arch} 测试版

这是可连接校园网的真实客户端，不是 Demo；无需安装 Node.js 或 Rust。
适用于 Apple Silicon（M 系列芯片）、macOS 12 或更新版本。

## 开始测试

1. 先退出旧 Demo / 旧版客户端，避免单实例机制唤起旧窗口。
2. 解压后打开 `DGCU Portal.app`。如需启用登录启动，先将它移动到“应用程序”目录，再开启后台服务。
3. 在“设置”输入校园网账号、密码。首次建议保持“仅一次会话”，打开日志，暂不开启后台服务与自动重拨。
4. 回到“网络”点击“上线”。程序会探测 Portal，依次显示认证和代拨状态。
5. 若模板和自动探测都不可用，点击认证后台图标打开后台；Portal URL 仍可在高级连接设置中手工填入。不要使用旧 HAR 里的 IP/MAC 参数。
6. 在“设置 → 开启日志 → 查看 DGCU CLI 日志”查看过程；日志只在内存保留，关闭日志开关会清空。
7. 流量只取校园网后台计费数据，更新间隔由服务端决定；等待更新不是本机实时速度为零。
8. 点击同一按钮的“下线”状态。无法唯一绑定时选择要下线的会话，不会自动踢其他设备。

## 已连接但看不到流量

可打开“管理会话”并点击“仅登录后台”，再选择需要管理的会话。
如果后台尚未上报新会话，客户端会继续查询；多个新会话需要手动选择。

## CLI

在本目录终端运行：
```
./dgcu-cli --log connect --one-session
./dgcu-cli sessions
./dgcu-cli disconnect <会话ID>
```
密码使用隐藏输入，不通过命令参数传递。CLI 的 `--log` 输出脱敏核心事件至 stderr。

## 测试反馈

仓库：https://github.com/miaoermua/dgcu-portal
请反馈失败阶段、错误提示、macOS 版本，附可见的脱敏日志即可。
不要附密码、Cookie 或未脱敏 HAR。

## 包的验证范围

这是本地 ad-hoc 签名测试包，没有 Apple Developer ID 签名或公证。
包含内嵌页面，可以离线启动；真实校园网认证需要你在校园网现场验证。
macOS/Windows/Linux 代码共享，但本包仅提供本机 macOS {arch} 构建。
没有自动安装系统服务、没有使用任何 HAR 内的账号登录。
系统 TUN/VPN 仍影响系统路由，“绕过程序代理”不会修改这些设置。
'''
(folder / '测试说明.md').write_text(guide)
archive = args.output.resolve() / (name + '.zip')
if archive.exists():
    raise SystemExit(f'Refusing to overwrite existing archive: {archive}')
subprocess.run(['ditto', '-c', '-k', '--sequesterRsrc', '--keepParent', str(folder), str(archive)], check=True)
digest = hashlib.sha256(archive.read_bytes()).hexdigest()
(args.output.resolve() / (name + '.sha256')).write_text(f'{digest}  {archive.name}\n')
print(archive)
