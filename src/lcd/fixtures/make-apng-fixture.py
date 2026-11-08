"""Independent tiny APNG fixture, using PNG chunks and Python stdlib only."""
import struct
import zlib
from pathlib import Path


def chunk(kind, data):
    return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))


def pixels(color):
    return zlib.compress((b"\x00" + bytes(color) * 2) * 2)


data = b"\x89PNG\r\n\x1a\n"
data += chunk(b"IHDR", struct.pack(">IIBBBBB", 2, 2, 8, 6, 0, 0, 0))
data += chunk(b"acTL", struct.pack(">II", 2, 0))
data += chunk(b"fcTL", struct.pack(">IIIIIHHBB", 0, 2, 2, 0, 0, 1, 10, 0, 0))
data += chunk(b"IDAT", pixels([255, 0, 0, 255]))
data += chunk(b"fcTL", struct.pack(">IIIIIHHBB", 1, 2, 2, 0, 0, 1, 5, 0, 0))
data += chunk(b"fdAT", struct.pack(">I", 2) + pixels([0, 0, 255, 255]))
data += chunk(b"IEND", b"")
Path("src/lcd/fixtures/two-frame.apng").write_bytes(data)
