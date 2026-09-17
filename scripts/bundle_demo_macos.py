"""Create a local unsigned demo app from an already-built binary (no installation)."""
import plistlib
import shutil
from pathlib import Path

root = Path(__file__).resolve().parents[1]
app = root / 'target/demo/DGCU Portal Demo.app'
macos = app / 'Contents/MacOS'
macos.mkdir(parents=True, exist_ok=True)
shutil.copy2(root / 'target/debug/portal-gui', macos / 'dgcu-portal-demo')
with (app / 'Contents/Info.plist').open('wb') as stream:
    plistlib.dump({
        'CFBundleExecutable': 'dgcu-portal-demo',
        'CFBundleIdentifier': 'net.dgcu.portal.demo',
        'CFBundleName': 'DGCU Portal Demo',
        'CFBundleDisplayName': 'DGCU Portal Demo',
        'CFBundlePackageType': 'APPL',
        'CFBundleVersion': '0.1.0',
        'CFBundleShortVersionString': '0.1.0',
        'NSHighResolutionCapable': True,
    }, stream)
print(app)
