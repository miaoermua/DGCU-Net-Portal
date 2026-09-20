"""Package release binaries into a portable Windows / Linux archive.

Build first, for example:

    cargo build --release -p portal-gui -p portal-cli --features portal-gui/custom-protocol --locked
    python3 scripts/package_portable.py --platform linux --arch x86_64 --output target/packages

The archive contains the real GUI client, the CLI, the license and a short
Chinese test guide. No installer, no service registration, no code signing.
"""
import argparse
import hashlib
import json
import tarfile
import zipfile
from pathlib import Path

parser = argparse.ArgumentParser()
parser.add_argument('--platform', choices=['windows', 'linux'], required=True)
parser.add_argument('--arch', required=True, help='包名中使用的架构名，例如 x86_64 / aarch64')
parser.add_argument('--output', type=Path, required=True)
parser.add_argument('--binary-dir', type=Path, default=None,
                    help='cargo 产物目录（默认 target/release，交叉编译时传 target/<triple>/release）')
args = parser.parse_args()

root = Path(__file__).resolve().parents[1]
config = json.loads((root / 'crates/portal-gui/tauri.conf.json').read_text())
version = config['version']
binary_dir = (args.binary_dir or Path('target/release')).resolve()

exe_suffix = '.exe' if args.platform == 'windows' else ''
files = {
    f'dgcu-portal{exe_suffix}': binary_dir / f'portal-gui{exe_suffix}',
    f'dgcu-cli{exe_suffix}': binary_dir / f'portal-cli{exe_suffix}',
    'LICENSE': root / 'LICENSE',
}
for label, path in files.items():
    if not path.exists():
        raise SystemExit(f'缺少构建产物：{path}（请先执行 cargo build --release）')

output = args.output.resolve()
output.mkdir(parents=True, exist_ok=True)
name = f'DGCU-Portal-{version}-{args.platform}-{args.arch}'
folder = output / name
if folder.exists():
    raise SystemExit(f'Refusing to overwrite existing package: {folder}')
folder.mkdir(parents=True)

if args.platform == 'windows':
    shell_lines = '''
在本目录 PowerShell 中运行：
```
.\\dgcu-cli.exe --log connect --one-session
.\\dgcu-cli.exe sessions
.\\dgcu-cli.exe disconnect <会话ID>
```
'''
    extra = '''Windows 版本是免安装的绿色包，直接双击 `dgcu-portal.exe` 即可。
系统托盘图标位于任务栏右下角，右键可以快速上线 / 下线。
“后台服务”在 Windows 上表现为当前用户的登录任务，不会写入系统级服务。
如果 Windows Defender 或 SmartScreen 提示未知发布者，这是未签名构建的正常提示。'''
else:
    shell_lines = '''
在本目录终端运行：
```
./dgcu-cli --log connect --one-session
./dgcu-cli sessions
./dgcu-cli disconnect <会话ID>
```
'''
    extra = '''Linux 版本是免安装的绿色包，直接运行 `./dgcu-portal` 即可。
桌面环境需要提供 WebKitGTK 4.1 与 Ayatana AppIndicator，例如 Debian/Ubuntu：
```
sudo apt install libwebkit2gtk-4.1-0 libayatana-appindicator3-1
```
Arch Linux 可以直接用同一个 Release 里的 `dgcu-portal-*.pkg.tar.zst`：
```
sudo pacman -U dgcu-portal-*.pkg.tar.zst
```
“后台服务”在 Linux 上表现为当前用户的 systemd user service。
无桌面环境时可以使用 `./dgcu-cli` 完成认证。'''

guide = f'''# DGCU Portal {version} · {args.platform} {args.arch} 测试版

这是可连接校园网的真实客户端，不是 Demo；无需安装 Node.js 或 Rust。

## 开始测试

1. 先退出旧版客户端，避免单实例机制唤起旧窗口。
2. 启动程序后进入“设置”，输入校园网账号、密码。首次建议保持“仅一次会话”，打开日志，暂不开启后台服务与自动重拨。
3. 回到“网络”点击“上线”。程序会探测 Portal，依次显示认证和代拨状态。
4. 若模板和自动探测都不可用，点击认证后台图标打开后台；Portal URL 仍可在高级连接设置中手工填入。不要使用旧 HAR 里的 IP/MAC 参数。
5. 在“设置 → 开启日志 → 查看 DGCU CLI 日志”查看过程；日志只在内存保留，关闭日志开关会清空。
6. 流量只取校园网后台计费数据，更新间隔由服务端决定；等待更新不是本机实时速度为零。
7. 点击同一按钮的“下线”状态。无法唯一绑定时选择要下线的会话，不会自动踢其他设备。

## 平台说明

{extra}

## CLI
{shell_lines}
密码使用隐藏输入，不通过命令参数传递。CLI 的 `--log` 输出脱敏核心事件至 stderr。

## 测试反馈

仓库：https://github.com/miaoermua/dgcu-portal
请反馈失败阶段、错误提示、系统版本，附可见的脱敏日志即可。
不要附密码、Cookie 或未脱敏 HAR。

## 包的验证范围

这是 CI 产出的未签名测试包，没有平台厂商签名或公证。
包含内嵌页面，可以离线启动；真实校园网认证需要你在校园网现场验证。
macOS/Windows/Linux 代码共享，但本包仅提供 {args.platform} {args.arch} 构建。
没有自动安装系统服务、没有使用任何 HAR 内的账号登录。
系统 TUN/VPN 仍影响系统路由，“绕过程序代理”不会修改这些设置。
'''

(folder / '测试说明.md').write_text(guide)
for label, path in files.items():
    if args.platform == 'windows':
        (folder / label).write_bytes(path.read_bytes())
    else:
        target = folder / label
        target.write_bytes(path.read_bytes())
        target.chmod(0o755 if label.startswith('dgcu-') else 0o644)

if args.platform == 'windows':
    archive = output / (name + '.zip')
    if archive.exists():
        raise SystemExit(f'Refusing to overwrite existing archive: {archive}')
    with zipfile.ZipFile(archive, 'w', zipfile.ZIP_DEFLATED, compresslevel=9) as stream:
        for path in sorted(folder.rglob('*')):
            if path.is_file():
                stream.write(path, Path(name) / path.relative_to(folder))
else:
    archive = output / (name + '.tar.gz')
    if archive.exists():
        raise SystemExit(f'Refusing to overwrite existing archive: {archive}')
    def normalize(info):
        info.uid, info.gid = 0, 0
        info.uname, info.gname = 'root', 'root'
        return info

    with tarfile.open(archive, 'w:gz') as stream:
        stream.add(folder, arcname=name, filter=normalize)

digest = hashlib.sha256(archive.read_bytes()).hexdigest()
(output / (name + '.sha256')).write_text(f'{digest}  {archive.name}\n')
print(archive)
