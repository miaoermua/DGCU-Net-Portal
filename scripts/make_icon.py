"""Generate an original RGBA icon, without third-party art or image dependencies."""
import struct
import zlib
from pathlib import Path

size = 64
pixels = bytearray()
for y in range(size):
    pixels.append(0)
    for x in range(size):
        inside = 7 <= x < 57 and 7 <= y < 57
        mark = (18 <= x < 24 and 18 <= y < 46) or (24 <= x < 39 and (18 <= y < 24 or 40 <= y < 46)) or (39 <= x < 46 and 24 <= y < 40)
        pixels.extend((17, 27, 46, 255) if mark else ((92, 204 - y, 196 + x // 3, 255) if inside else (0, 0, 0, 0)))

def chunk(kind, data):
    return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data))

png = b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', size, size, 8, 6, 0, 0, 0)) + chunk(b'IDAT', zlib.compress(pixels)) + chunk(b'IEND', b'')
path = Path(__file__).resolve().parents[1] / 'crates/portal-gui/icons/icon.png'
path.parent.mkdir(parents=True, exist_ok=True)
path.write_bytes(png)
