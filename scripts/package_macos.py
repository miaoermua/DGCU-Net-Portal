"""Package release binaries into a local ad-hoc signed macOS test application.

No installation or service registration is performed. Build first with:
    cargo build --release -p portal-gui -p portal-cli --features portal-gui/custom-protocol --locked

打包是幂等的：重复运行会先清掉上一次的同名产物再重新生成。
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
