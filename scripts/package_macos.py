"""Package release binaries into a local ad-hoc signed macOS test application.

No installation or service registration is performed. Build first with:
    cargo build --release -p portal-gui -p portal-cli --features portal-gui/custom-protocol --locked

打包是幂等的：重复运行会先清掉上一次的同名产物再重新生成。

With --dmg it also writes an installer disk image: the app on the left, an Applications
shortcut on the right, over a background drawn from packaging/macos/background.svg on the
runner itself. The app must already be signed, so the image is made after codesign and the
bundle it holds is the signed one.
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
parser.add_argument('--arch', default=platform.machine(), help='包名中使用的架构名（默认本机架构）')
parser.add_argument('--binary-dir', type=Path, default=None, help='cargo 产物目录（默认 target/release，交叉编译时传 target/<triple>/release）')
parser.add_argument('--dmg', action='store_true', help='额外出一个安装器磁盘映像（.dmg）')
parser.add_argument('--volume', default='DGCU-Net-Portal', help='dmg 的卷名（默认 DGCU-Net-Portal）')
args = parser.parse_args()
root = Path(__file__).resolve().parents[1]
config = json.loads((root / 'crates/portal-gui/tauri.conf.json').read_text(encoding='utf-8'))
version = config['version']
arch = args.arch
binary_dir = (args.binary_dir or Path('target/release')).resolve()
name = f'DGCU-Net-Portal-{version}-macos-{arch}'
output = args.output.resolve()
folder = output / name
archive = output / (name + '.zip')
# 先清掉上一轮的同名产物。CI 用 Swatinem/rust-cache 缓存整个 target/，
# 上一轮运行留下的空目录会在新 runner 上被还原出来，这里若直接报错就会让流水线失败。
if folder.exists():
    shutil.rmtree(folder)
archive.unlink(missing_ok=True)
(output / (name + '.sha256')).unlink(missing_ok=True)
disk_image = output / (name + '.dmg')
disk_image.unlink(missing_ok=True)
(output / (name + '.dmg.sha256')).unlink(missing_ok=True)
binary = binary_dir / 'portal-gui'
cli = binary_dir / 'portal-cli'
for file in [binary, cli]:
    subprocess.run(['file', str(file)], check=True)
app = folder / 'DGCU-Net-Portal.app'
macos = app / 'Contents/MacOS'
resources = app / 'Contents/Resources'
macos.mkdir(parents=True)
resources.mkdir(parents=True)
shutil.copy2(binary, macos / 'portal-gui')
shutil.copy2(cli, macos / 'portal-cli')
shutil.copy2(cli, folder / 'portal-cli')
shutil.copy2(root / 'crates/portal-gui/icons/icon.icns', resources / 'icon.icns')
shutil.copy2(root / 'LICENSE', folder / 'LICENSE')
with (app / 'Contents/Info.plist').open('wb') as stream:
    plistlib.dump({
        'CFBundleExecutable': 'portal-gui',
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
# 先给 bundle 内的嵌套可执行文件（portal-cli）做 ad-hoc 签名，再签整个 bundle。
# codesign 在签 bundle、或签 bundle 的主可执行文件（portal-gui）时都会校验所有嵌套代码；
# 交叉编译 x86_64 的产物没有链接器附带的 ad-hoc 签名，若不先补签嵌套文件，
# 就会以 “code object is not signed at all / In subcomponent” 失败。
subprocess.run(['codesign', '--force', '--sign', '-', str(macos / 'portal-cli')], check=True)
subprocess.run(['codesign', '--force', '--sign', '-', str(app)], check=True)
subprocess.run(['codesign', '--verify', '--deep', '--strict', str(app)], check=True)
subprocess.run(['codesign', '--force', '--sign', '-', str(folder / 'portal-cli')], check=True)
subprocess.run(['ditto', '-c', '-k', '--sequesterRsrc', '--keepParent', str(folder), str(archive)], check=True)
digest = hashlib.sha256(archive.read_bytes()).hexdigest()
(output / (name + '.sha256')).write_text(f'{digest}  {archive.name}\n', encoding='utf-8')
print(archive)

if args.dmg:
    # dmgbuild lives in a venv of its own under target/, made here, so a runner and a machine
    # take the same path and neither has its python written to. The venv is asked whether it is
    # still a venv, not whether its files are there: CI 用 Swatinem/rust-cache 缓存整个 target/，
    # 上一轮运行留下的 venv 可能在新 runner 上被还原出来，而它的 python 已经不在 venv 里了，
    # 这时 pip 会以系统的身份运行并被拒绝，所以答案是「不是」就整个重建。
    venv = root / 'target/installer/venv'
    font = root / 'target/installer/Kalam-Regular.ttf'
    venv.parent.mkdir(parents=True, exist_ok=True)
    # sys.exit(prefix == base_prefix) 在两者相等时退出码为 1，也就是「不是 venv」，这时重建；
    # 相等为假说明确实在 venv 里，退出码 0，留着。
    probe = None
    if (venv / 'bin/python3').exists():
        probe = subprocess.run([str(venv / 'bin/python3'), '-c', 'import sys; sys.exit(sys.prefix == sys.base_prefix)'], capture_output=True)
    if probe is None or probe.returncode != 0:
        shutil.rmtree(venv, ignore_errors=True)
        subprocess.run(['python3', '-m', 'venv', str(venv)], check=True)
    python = venv / 'bin/python3'
    # 网络偶尔会把字体下载掐断，重试三次再放弃。--retry-all-errors 是必需的：
    # 没有它，curl 只重试超时这类错误，SSL 握手中断（exit 35）会直接失败。
    if not font.exists():
        subprocess.run(['curl', '-fsSL', '--retry', '3', '--retry-all-errors', '--retry-delay', '2', '--max-time', '60',
            '-o', str(font), 'https://raw.githubusercontent.com/google/fonts/main/ofl/kalam/Kalam-Regular.ttf'], check=True)
    if subprocess.run([str(python), '-c', 'import dmgbuild'], capture_output=True).returncode != 0:
        # 同一个网络也会把 pip 的连接掐断，所以给它自己的重试。
        subprocess.run([str(python), '-m', 'pip', 'install', '--quiet', '--retries', '5', '--timeout', '60', 'dmgbuild'], check=True)
    # 背景图在 runner 上用 AppKit 渲染，2x。字体只为这一次渲染注册，机器上不必安装。
    picture = root / 'target/installer/background.png'
    subprocess.run(['swift', str(root / 'packaging/macos/render.swift'), str(root / 'packaging/macos/background.svg'), str(picture), '2', str(font)], check=True)
    subprocess.run([str(python), '-m', 'dmgbuild', '-s', str(root / 'packaging/macos/installer.py'),
        '-D', f'app={app}', '-D', f'background={picture}', args.volume, str(disk_image)], check=True)
    digest = hashlib.sha256(disk_image.read_bytes()).hexdigest()
    (output / (name + '.dmg.sha256')).write_text(f'{digest}  {disk_image.name}\n', encoding='utf-8')
    print(disk_image)
