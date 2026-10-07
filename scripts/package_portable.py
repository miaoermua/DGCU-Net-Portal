"""Package release binaries into a portable Windows / Linux archive.

Build first, for example:

    cargo build --release -p portal-gui -p portal-cli --features portal-gui/custom-protocol --locked
    python3 scripts/package_portable.py --platform linux --arch x86_64 --output target/packages

The archive contains the real GUI client, the CLI and the license.
No installer, no service registration, no code signing.

打包是幂等的：重复运行会先清掉上一次的同名产物再重新生成。
"""
import argparse
import hashlib
import json
import shutil
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
config = json.loads((root / 'crates/portal-gui/tauri.conf.json').read_text(encoding='utf-8'))
version = config['version']
binary_dir = (args.binary_dir or Path('target/release')).resolve()

exe_suffix = '.exe' if args.platform == 'windows' else ''
files = {
    f'portal-gui{exe_suffix}': binary_dir / f'portal-gui{exe_suffix}',
    f'portal-cli{exe_suffix}': binary_dir / f'portal-cli{exe_suffix}',
    'LICENSE': root / 'LICENSE',
}
for label, path in files.items():
    if not path.exists():
        raise SystemExit(f'缺少构建产物：{path}（请先执行 cargo build --release）')

output = args.output.resolve()
output.mkdir(parents=True, exist_ok=True)
name = f'DGCU-Net-Portal-{version}-{args.platform}-{args.arch}'
folder = output / name
# 先清掉上一轮的同名产物。CI 用 Swatinem/rust-cache 缓存整个 target/，
# 上一轮运行留下的空目录会在新 runner 上被还原出来，这里若直接报错就会让流水线失败。
if folder.exists():
    shutil.rmtree(folder)
folder.mkdir(parents=True)

for label, path in files.items():
    if args.platform == 'windows':
        (folder / label).write_bytes(path.read_bytes())
    else:
        target = folder / label
        target.write_bytes(path.read_bytes())
        # LICENSE 保持 0644，portal-gui / portal-cli 必须是可执行的 0755
        target.chmod(0o644 if label == 'LICENSE' else 0o755)

if args.platform == 'windows':
    archive = output / (name + '.zip')
    with zipfile.ZipFile(archive, 'w', zipfile.ZIP_DEFLATED, compresslevel=9) as stream:
        for path in sorted(folder.rglob('*')):
            if path.is_file():
                stream.write(path, Path(name) / path.relative_to(folder))
else:
    archive = output / (name + '.tar.gz')
    def normalize(info):
        info.uid, info.gid = 0, 0
        info.uname, info.gname = 'root', 'root'
        return info

    with tarfile.open(archive, 'w:gz') as stream:
        stream.add(folder, arcname=name, filter=normalize)

digest = hashlib.sha256(archive.read_bytes()).hexdigest()
(output / (name + '.sha256')).write_text(f'{digest}  {archive.name}\n', encoding='utf-8')
print(archive)
