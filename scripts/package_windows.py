"""把 Windows 产物打成 NSIS 安装器。

GUI 与 CLI 必须同目录（GUI 沿自身路径的祖先目录找同包的 daemon），
所以这里把两个 exe 与 LICENSE 暂存到一个目录，再让 NSIS 整目录收进去。

先编译再运行：

    cargo build --release -p portal-gui -p portal-cli --features portal-gui/custom-protocol --target x86_64-pc-windows-msvc
    python scripts/package_windows.py --arch x86_64 --output target/packages \\
        --binary-dir target/x86_64-pc-windows-msvc/release

需要 PATH 里有 makensis（Windows 上 `choco install nsis`，macOS 上 `brew install nsis`）。
产物是 `DGCU-Net-Portal-<version>-windows-<arch>-setup.exe`。校验和不在这里生成，
统一由 Release 的 SHA256SUMS.txt 覆盖。
"""
import argparse
import json
import shutil
import subprocess
import tempfile
from pathlib import Path

parser = argparse.ArgumentParser()
parser.add_argument('--output', type=Path, required=True)
parser.add_argument('--arch', required=True, help='包名中使用的架构名，例如 x86_64 / aarch64')
parser.add_argument('--binary-dir', type=Path, default=None,
                    help='cargo 产物目录（默认 target/release，交叉编译时传 target/<triple>/release）')
parser.add_argument('--makensis', default='makensis', help='makensis 可执行文件（默认从 PATH 找）')
args = parser.parse_args()

root = Path(__file__).resolve().parents[1]
config = json.loads((root / 'crates/portal-gui/tauri.conf.json').read_text(encoding='utf-8'))
version = config['version']
binary_dir = (args.binary_dir or Path('target/release')).resolve()
output = args.output.resolve()
output.mkdir(parents=True, exist_ok=True)

# NSIS 3 靠 BOM 判断脚本编码：没有 BOM 时按系统 ANSI 代码页解码。macOS/Linux 的
# locale 是 UTF-8，碰巧读得对；Windows runner 是 CP1252，安装器里的中文会变成
# "ç¨‹åºæ–‡ä»¶" 那种乱码，而且编译不报错。编辑器另存时很容易把 BOM 丢掉，
# 而这种回归在本机 macOS 上完全看不出来，所以构建前直接检查。
installer_script = root / 'packaging/windows/installer.nsi'
if not installer_script.read_bytes().startswith(b'\xef\xbb\xbf'):
    raise SystemExit(
        f'{installer_script} 缺少 UTF-8 BOM —— NSIS 在 Windows 上按 CP1252 解码，'
        '安装器里的中文会显示成乱码（macOS 本地编译反而正常，很难发现）。\n'
        '修复：用编辑器「另存为 → UTF-8 with BOM」。'
    )

# 四段版本：VIProductVersion 只接受 x.x.x.x。
parts = (version.split('.') + ['0', '0', '0'])[:4]
version4 = '.'.join(parts)

gui = binary_dir / 'portal-gui.exe'
cli = binary_dir / 'portal-cli.exe'
for path in (gui, cli):
    if not path.exists():
        raise SystemExit(f'缺少构建产物：{path}（请先 cargo build --release --target ...）')

name = f'DGCU-Net-Portal-{version}-windows-{args.arch}-setup'
installer = output / (name + '.exe')
# 打包是幂等的：重复运行先清掉上一次的产物。
installer.unlink(missing_ok=True)
(output / (name + '.sha256')).unlink(missing_ok=True)

# 暂存目录让 NSIS 只认一个路径，不用在 .nsi 里写一堆相对层级。
staging = Path(tempfile.mkdtemp(prefix='dgcu-nsis-'))
try:
    shutil.copy2(gui, staging / 'portal-gui.exe')
    shutil.copy2(cli, staging / 'portal-cli.exe')
    shutil.copy2(root / 'LICENSE', staging / 'LICENSE')

    command = [
        args.makensis,
        '-V2',
        f'-DVERSION={version}',
        f'-DVERSION4={version4}',
        f'-DARCH={args.arch}',
        f'-DBINARYDIR={staging}',
        f'-DICONFILE={root / "crates/portal-gui/icons/icon.ico"}',
        f'-DOUTFILE={installer}',
        str(installer_script),
    ]
    result = subprocess.run(command, capture_output=True, text=True)
    if result.returncode != 0:
        print(result.stdout)
        print(result.stderr)
        raise SystemExit('makensis 失败')
    if not installer.exists():
        raise SystemExit(f'makensis 未产出 {installer}')
finally:
    shutil.rmtree(staging, ignore_errors=True)

# 不产出单独的 .sha256：校验和统一进 Release 的 SHA256SUMS.txt。
print(installer)
