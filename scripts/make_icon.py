"""Generate application icons from the user-provided xiaowei.png. Requires Pillow.

The source image stays unchanged. White is made transparent only in the
macOS monochrome tray template; the app/Dock/Windows icons retain the artwork.

macOS 的托盘模板属于手工调过的资产，默认不重新生成；只有显式传入
--macos-tray 才会用当前源图覆盖 tray-template.png / tray-template.rgba。
"""
import argparse
from pathlib import Path
from PIL import Image, ImageChops

root = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--macos-tray", action="store_true",
                    help="用当前源图重新生成 macOS 托盘模板（默认保留现有文件）")
args = parser.parse_args()

source = Image.open(root / "xiaowei.png").convert("RGBA")
icons = root / "crates/portal-gui/icons"
icons.mkdir(parents=True, exist_ok=True)
for name, size in [("icon.png", 256), ("32x32.png", 32), ("128x128.png", 128), ("128x128@2x.png", 256)]:
    source.resize((size, size), Image.Resampling.LANCZOS).save(icons / name)
source.save(icons / "icon.ico", sizes=[(size, size) for size in (16, 24, 32, 48, 64, 128, 256)])
source.save(icons / "icon.icns")

if args.macos_tray:
    tray = source.resize((32, 32), Image.Resampling.LANCZOS)
    alpha = ImageChops.multiply(ImageChops.invert(tray.convert("L")), tray.getchannel("A"))
    template = Image.new("RGBA", (32, 32), (0, 0, 0, 0))
    template.putalpha(alpha)
    template.save(icons / "tray-template.png")
    (icons / "tray-template.rgba").write_bytes(template.tobytes())
