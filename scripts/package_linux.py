"""把 Linux 产物打成 .deb 或 .rpm。

与 Arch 那套一样，只做「安装布局」，不重新编译：二进制由 CI 的 linux job
产出后放进 --binary-dir，这里负责摆进包的目录结构再压成发行版格式。

    python3 scripts/package_linux.py --format deb --arch x86_64 \\
        --output target/packages --binary-dir target/release
    python3 scripts/package_linux.py --format rpm --arch x86_64 \\
        --output target/packages --binary-dir target/release

deb 用 Python 直接拼 ar 归档，不依赖 dpkg-deb，macOS 上也能验证；
rpm 需要 PATH 里有 rpmbuild（Ubuntu 上 `apt install rpm`，Fedora 上 rpm-build）。

GUI 与 CLI 必须装在同一目录：GUI 沿自身路径的祖先目录找同包的 daemon。
"""
import argparse
import gzip
import hashlib
import io
import json
import shutil
import subprocess
import tarfile
import tempfile
from pathlib import Path

parser = argparse.ArgumentParser()
parser.add_argument('--format', choices=['deb', 'rpm'], required=True)
parser.add_argument('--arch', required=True, help='cargo 侧架构名，例如 x86_64 / aarch64')
parser.add_argument('--output', type=Path, required=True)
parser.add_argument('--binary-dir', type=Path, default=None,
                    help='cargo 产物目录（默认 target/release）')
parser.add_argument('--rpmbuild', default='rpmbuild', help='rpmbuild 可执行文件')
args = parser.parse_args()

root = Path(__file__).resolve().parents[1]
config = json.loads((root / 'crates/portal-gui/tauri.conf.json').read_text(encoding='utf-8'))
version = config['version']
binary_dir = (args.binary_dir or Path('target/release')).resolve()
output = args.output.resolve()
output.mkdir(parents=True, exist_ok=True)

# 各发行版对架构的叫法不同，包名里必须用发行版那一套。
ARCH = {
    'x86_64': {'deb': 'amd64', 'rpm': 'x86_64'},
    'aarch64': {'deb': 'arm64', 'rpm': 'aarch64'},
}
if args.arch not in ARCH:
    raise SystemExit(f'不支持的架构：{args.arch}')

PACKAGE = 'dgcu-net-portal'
LICENSE_PATH = Path('usr/share/licenses') / PACKAGE / 'LICENSE'
# 固定时间戳，让同样的输入产出同样的包（tar 与 ar 都不写入当前时间）。
EPOCH = 0


def stage(target: Path) -> None:
    """按 FHS 布局摆好要装的文件。"""
    gui = binary_dir / 'portal-gui'
    cli = binary_dir / 'portal-cli'
    for path in (gui, cli):
        if not path.exists():
            raise SystemExit(f'缺少构建产物：{path}（请先 cargo build --release）')

    def put(relative: str, source: Path, mode: int) -> None:
        destination = target / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, destination)
        destination.chmod(mode)

    put('usr/bin/portal-gui', gui, 0o755)
    put('usr/bin/portal-cli', cli, 0o755)
    put('usr/share/applications/dgcu-portal.desktop',
        root / 'packaging/linux/dgcu-portal.desktop', 0o644)
    put('usr/share/icons/hicolor/256x256/apps/portal-gui.png',
        root / 'crates/portal-gui/icons/icon.png', 0o644)
    put(str(LICENSE_PATH), root / 'LICENSE', 0o644)


def gzipped_tar(base: Path, members: list) -> bytes:
    """打一个 tar.gz，所有条目的属主与时间都归一，保证可复现。"""
    raw = io.BytesIO()
    with tarfile.open(fileobj=raw, mode='w', format=tarfile.GNU_FORMAT) as tar:
        for path in members:
            arcname = './' + (path.relative_to(base).as_posix() if path != base else '')
            info = tar.gettarinfo(str(path), arcname=arcname)
            info.uid = info.gid = 0
            info.uname = info.gname = 'root'
            info.mtime = EPOCH
            # 根条目本身来自临时目录，权限是 0700，要按常规目录写回 0755。
            if path == base:
                info.mode = 0o755
            if path.is_file():
                with path.open('rb') as stream:
                    tar.addfile(info, stream)
            else:
                tar.addfile(info)
    packed = io.BytesIO()
    with gzip.GzipFile(fileobj=packed, mode='wb', mtime=EPOCH) as archive:
        archive.write(raw.getvalue())
    return packed.getvalue()


def ar_member(name: str, payload: bytes) -> bytes:
    """ar 归档成员头。deb 只认这一种老格式，字段宽度是固定的。"""
    header = (
        name.encode().ljust(16)
        + str(EPOCH).encode().ljust(12)
        + b'0'.ljust(6)
        + b'0'.ljust(6)
        + b'100644'.ljust(8)
        + str(len(payload)).encode().ljust(10)
        + b'`\n'
    )
    # 奇数长度要补一个换行，否则下一个成员头会错位。
    padding = b'\n' if len(payload) % 2 else b''
    return header + payload + padding


def build_deb(staging: Path) -> Path:
    installed_size = sum(
        path.stat().st_size for path in staging.rglob('*') if path.is_file()
    )
    control = (root / 'packaging/debian/control').read_text(encoding='utf-8')
    control = (
        control.replace('@VERSION@', version)
        # Installed-Size 的单位是 KiB，向上取整。
        .replace('@INSTALLED_SIZE@', str((installed_size + 1023) // 1024))
        .replace('@ARCH@', ARCH[args.arch]['deb'])
    )
    control_dir = Path(tempfile.mkdtemp(prefix='dgcu-deb-control-'))
    try:
        (control_dir / 'control').write_text(control, encoding='utf-8')
        # 文档目录：Debian 规定每个包都要有 copyright。
        doc = staging / 'usr/share/doc' / PACKAGE
        doc.mkdir(parents=True, exist_ok=True)
        shutil.copy2(root / 'LICENSE', doc / 'copyright')
        (doc / 'copyright').chmod(0o644)

        control_tar = gzipped_tar(control_dir, [control_dir / 'control'])
        data_tar = gzipped_tar(
            staging,
            [staging] + sorted(p for p in staging.rglob('*') if p != staging),
        )
    finally:
        shutil.rmtree(control_dir, ignore_errors=True)

    archive = output / f'{PACKAGE}_{version}_{ARCH[args.arch]["deb"]}.deb'
    archive.unlink(missing_ok=True)
    with archive.open('wb') as stream:
        stream.write(b'!<arch>\n')
        stream.write(ar_member('debian-binary', b'2.0\n'))
        stream.write(ar_member('control.tar.gz', control_tar))
        stream.write(ar_member('data.tar.gz', data_tar))
    return archive


def build_rpm(staging: Path) -> Path:
    if not shutil.which(args.rpmbuild):
        raise SystemExit(f'找不到 {args.rpmbuild}（Ubuntu 上 apt install rpm）')
    topdir = Path(tempfile.mkdtemp(prefix='dgcu-rpm-'))
    try:
        for name in ('SPECS', 'SOURCES', 'BUILD', 'BUILDROOT', 'RPMS', 'SRPMS'):
            (topdir / name).mkdir()
        spec = (root / 'packaging/rpm/dgcu-net-portal.spec').read_text(encoding='utf-8')
        (topdir / 'SPECS/dgcu-net-portal.spec').write_text(
            spec.replace('@VERSION@', version), encoding='utf-8')

        command = [
            args.rpmbuild, '-bb', str(topdir / 'SPECS/dgcu-net-portal.spec'),
            '--target', ARCH[args.arch]['rpm'],
            '--define', f'_topdir {topdir}',
            '--define', f'stagedir {staging}',
            '--define', '_binary_payload w9.gzdio',
            # 打包装的是已编好的二进制，不需要再跑 debuginfo 与 strip 那一套；
            # Ubuntu 上也没有对应的 brp 脚本，留着只会报错。
            '--define', 'debug_package %{nil}',
            '--define', '__os_install_post %{nil}',
        ]
        result = subprocess.run(command, capture_output=True, text=True)
        if result.returncode != 0:
            print(result.stdout)
            print(result.stderr)
            raise SystemExit('rpmbuild 失败')

        built = sorted((topdir / 'RPMS').rglob('*.rpm'))
        if not built:
            raise SystemExit('rpmbuild 未产出 .rpm')
        archive = output / built[-1].name
        archive.unlink(missing_ok=True)
        shutil.copy2(built[-1], archive)
        return archive
    finally:
        shutil.rmtree(topdir, ignore_errors=True)


staging = Path(tempfile.mkdtemp(prefix='dgcu-pkg-'))
try:
    stage(staging)
    archive = build_deb(staging) if args.format == 'deb' else build_rpm(staging)
finally:
    shutil.rmtree(staging, ignore_errors=True)

digest = hashlib.sha256(archive.read_bytes()).hexdigest()
(archive.parent / (archive.name + '.sha256')).write_text(
    f'{digest}  {archive.name}\n', encoding='utf-8')
print(archive)
