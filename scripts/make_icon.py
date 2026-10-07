"""Generate application icons from docs/xiaowei.png. Requires Pillow.

The source image stays unchanged. White is made transparent only in the
macOS monochrome tray template; the app/Dock/Windows icons retain the artwork.

macOS 的托盘模板属于手工调过的资产，默认不重新生成；只有显式传入
--macos-tray 才会用当前源图覆盖 tray-template.png / tray-template.rgba。
"""
import argparse
import struct
from pathlib import Path
from PIL import Image, ImageChops

root = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--macos-tray", action="store_true",
                    help="用当前源图重新生成 macOS 托盘模板（默认保留现有文件）")
args = parser.parse_args()

source_path = root / "docs/xiaowei.png"
if not source_path.is_file():
    raise SystemExit(f"缺少源图：{source_path}")
source = Image.open(source_path).convert("RGBA")
icons = root / "crates/portal-gui/icons"
icons.mkdir(parents=True, exist_ok=True)
for name, size in [("icon.png", 256), ("32x32.png", 32), ("128x128.png", 128), ("128x128@2x.png", 256)]:
    source.resize((size, size), Image.Resampling.LANCZOS).save(icons / name)


def largest_ico_entry_first(path: Path) -> None:
    """把 ICO 的目录表按尺寸从大到小重排。

    tauri-codegen 取的是 IconDir::entries()[0]（见 tauri-codegen/src/image.rs），它被塞进
    tao 的 window_icon，最终只写进 WM_SETICON 的 ICON_SMALL；而 tauri 从不设置 taskbar_icon，
    tao 建窗口时反而把 ICON_BIG 清成 NULL，任务栏与 Alt+Tab 于是回落到这张小图。Pillow 写
    ICO 无视传入顺序、固定升序排列，第 0 档永远是 16x16，被放大到 32/48/64 就会糊。
    这里只重排 16 字节的目录项，各条目指向的数据块原地不动，偏移量无需改动。
    """
    data = path.read_bytes()
    count = struct.unpack("<H", data[4:6])[0]
    entries = [data[6 + 16 * i:22 + 16 * i] for i in range(count)]
    blobs = data[6 + 16 * count:]
    entries.sort(key=lambda entry: entry[0] or 256, reverse=True)
    path.write_bytes(data[:6] + b"".join(entries) + blobs)


source.save(icons / "icon.ico", sizes=[(size, size) for size in (16, 24, 32, 48, 64, 128, 256)])
largest_ico_entry_first(icons / "icon.ico")
source.save(icons / "icon.icns")
# Windows 托盘需要独立的位图：icon.ico 的第 0 档（256）要留给窗口/任务栏，托盘要的是小图。
(icons / "tray-win.rgba").write_bytes(source.resize((32, 32), Image.Resampling.LANCZOS).tobytes())

if args.macos_tray:
    tray = source.resize((32, 32), Image.Resampling.LANCZOS)
    alpha = ImageChops.multiply(ImageChops.invert(tray.convert("L")), tray.getchannel("A"))
    template = Image.new("RGBA", (32, 32), (0, 0, 0, 0))
    template.putalpha(alpha)
    template.save(icons / "tray-template.png")
    (icons / "tray-template.rgba").write_bytes(template.tobytes())
