"""生成 Release 说明里的「快捷下载」表。

softprops/action-gh-release 在 `generate_release_notes: true` 时会把 body
拼在自动生成的更新说明**前面**（官方文档：the body will be pre-pended），
所以这里只负责产出表格本身，提交历史那部分仍由 GitHub 生成。

链接用「当前 tag + 资产名」拼，不用 latest：资产名里带版本号，
用 latest 会让下载链接和页面上的版本对不上。

    python3 scripts/release_notes.py --assets release --repo owner/name --tag v0.5.13
"""
import argparse
import re
from pathlib import Path

parser = argparse.ArgumentParser()
parser.add_argument('--assets', type=Path, required=True, help='放产物的目录')
parser.add_argument('--repo', required=True, help='owner/name')
parser.add_argument('--tag', required=True, help='例如 v0.5.13')
parser.add_argument('--output', type=Path, default=Path('release-notes.md'))
args = parser.parse_args()

# (正则, 展示名模板, 排序权重)。顺序即表格顺序：先放各平台的主下载，
# 再放发行版包管理器的产物。
RULES = [
    (r'-windows-(?P<arch>[^-]+)-setup\.exe$', 'Windows {arch}（安装版）', 10),
    (r'-windows-(?P<arch>[^-]+)\.zip$', 'Windows {arch}（便携版）', 20),
    (r'-macos-(?P<arch>[^-]+)\.dmg$', 'macOS {mac}', 30),
    # 便携版指 AppImage：单文件、下载即可运行，不需要装。
    (r'-linux-(?P<arch>[^-]+)\.AppImage$', 'Linux {arch}（便携版）', 40),
    (r'\.pkg\.tar\.zst$', 'Arch Linux', 50),
]
MAC_LABEL = {'arm64': 'Apple Silicon', 'x86_64': 'Intel'}

# 表格只放各平台的主入口，不求把资产列全。下面这些照常上传到 Release，
# 只是不进表，需要的人仍能在资产列表里找到：
#   Windows arm64 的安装版与便携版 —— 用的人少，列出来只是多两行；
#   Linux arm64 的 AppImage —— 同上，arm 桌面本来就少；
#   Intel Mac 的 dmg —— Apple Silicon 占绝大多数，并列两行反而让人多犹豫一次；
#   Linux 的 tar.gz / deb / rpm —— 同类已经由 AppImage 与 Arch 包覆盖，
#   全列出来表格会长一倍。
SKIP = [
    r'-windows-aarch64-setup\.exe$',
    r'-windows-aarch64\.zip$',
    r'-linux-aarch64\.AppImage$',
    r'-macos-x86_64\.dmg$',
    r'-linux-[^-]+\.tar\.gz$',
    r'\.deb$',
    r'\.rpm$',
]


def describe(name: str):
    for pattern, label, order in RULES:
        match = re.search(pattern, name)
        if match:
            fields = match.groupdict()
            fields['mac'] = MAC_LABEL.get(fields.get('arch', ''), fields.get('arch', ''))
            return label.format(**fields), order
    return None, 999


base = f'https://github.com/{args.repo}/releases/download/{args.tag}'
rows, checksum = [], None
for path in sorted(args.assets.iterdir()):
    if not path.is_file():
        continue
    if path.name == 'SHA256SUMS.txt':
        checksum = path.name
        continue
    if any(re.search(pattern, path.name) for pattern in SKIP):
        continue
    label, order = describe(path.name)
    if label is None:
        continue
    size = path.stat().st_size / 1024 / 1024
    rows.append((order, label, path.name, size))

if not rows:
    raise SystemExit(f'{args.assets} 里没有可识别的产物')

rows.sort(key=lambda row: (row[0], row[1]))
lines = [
    '## 快捷下载',
    '',
    '| 平台 | 文件 | 大小 |',
    '| --- | --- | --- |',
]
for _, label, name, size in rows:
    lines.append(f'| {label} | [{name}]({base}/{name}) | {size:.1f} MB |')
if checksum:
    lines.append('')
    lines.append(f'全部产物的校验和见 [{checksum}]({base}/{checksum})。')

args.output.write_text('\n'.join(lines) + '\n', encoding='utf-8')
print(f'已写入 {args.output}（{len(rows)} 个产物）')
