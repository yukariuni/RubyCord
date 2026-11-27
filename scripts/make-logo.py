"""Builds the logo blobs crates/ui-desktop/src/logo/<size>.rgba from a source PNG.

    python scripts/make-logo.py path/to/logo.png [preview.png]

The source must be an 8-bit RGBA PNG. The script drops specks too small to be part of the logo,
crops to what is left, pads it to a square, and writes the straight-alpha RGBA pixels at each
size the client uses. Needs numpy only.
"""

import struct
import sys
import zlib
from pathlib import Path

import numpy as np

SIZES = [16, 32, 48, 64, 128]
OUT = Path(__file__).resolve().parent.parent / "crates" / "ui-desktop" / "src" / "logo"


def read_png(path):
    data = Path(path).read_bytes()
    assert data[:8] == b"\x89PNG\r\n\x1a\n", "not a PNG"
    pos, idat, header = 8, b"", None
    while pos < len(data):
        (length,) = struct.unpack(">I", data[pos:pos + 4])
        kind = data[pos + 4:pos + 8]
        body = data[pos + 8:pos + 8 + length]
        if kind == b"IHDR":
            header = struct.unpack(">IIBBBBB", body)
        elif kind == b"IDAT":
            idat += body
        pos += 12 + length
    width, height, depth, color, _, _, interlace = header
    assert (depth, color, interlace) == (8, 6, 0), "need an 8-bit RGBA PNG without interlacing"
    raw = zlib.decompress(idat)
    stride = width * 4
    rows = np.zeros((height, stride), dtype=np.uint8)
    prev = np.zeros(stride, dtype=np.int32)
    at = 0
    for y in range(height):
        kind, line = raw[at], np.frombuffer(raw, dtype=np.uint8, count=stride, offset=at + 1).astype(np.int32)
        at += stride + 1
        if kind == 0:
            row = line
        elif kind == 1:
            row = line.copy()
            for i in range(4, stride):
                row[i] = (row[i] + row[i - 4]) & 255
        elif kind == 2:
            row = (line + prev) & 255
        elif kind == 3:
            row = line.copy()
            for i in range(stride):
                left = row[i - 4] if i >= 4 else 0
                row[i] = (row[i] + ((left + prev[i]) >> 1)) & 255
        elif kind == 4:
            row = line.copy()
            for i in range(stride):
                a = row[i - 4] if i >= 4 else 0
                b = prev[i]
                c = prev[i - 4] if i >= 4 else 0
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                pred = a if pa <= pb and pa <= pc else (b if pb <= pc else c)
                row[i] = (row[i] + pred) & 255
        else:
            raise ValueError(f"filter {kind}")
        rows[y] = row
        prev = row
    return rows.reshape(height, width, 4)


def shifted(mask, dy, dx):
    out = np.zeros_like(mask)
    h, w = mask.shape
    out[max(dy, 0):h + min(dy, 0), max(dx, 0):w + min(dx, 0)] = mask[max(-dy, 0):h + min(-dy, 0), max(-dx, 0):w + min(-dx, 0)]
    return out


def sweep(mask, radius, combine):
    """Combines the mask with itself shifted up to `radius` pixels along each axis, in turn."""
    for axis in ((1, 0), (0, 1)):
        out = mask.copy()
        for step in range(-radius, radius + 1):
            out = combine(out, shifted(mask, step * axis[0], step * axis[1]))
        mask = out
    return mask


def opened(mask, radius):
    """Erosion then dilation by a square, which removes anything narrower than it."""
    eroded = sweep(mask, radius, np.logical_and)
    return sweep(eroded, radius, np.logical_or)


def area_matrix(out_size, in_size):
    """Rows that average `in_size` samples down to `out_size`, with fractional edges."""
    matrix = np.zeros((out_size, in_size))
    scale = in_size / out_size
    for o in range(out_size):
        start, end = o * scale, (o + 1) * scale
        for i in range(int(start), min(int(np.ceil(end)), in_size)):
            matrix[o, i] = min(end, i + 1) - max(start, i)
    return matrix / scale


def main():
    source = sys.argv[1]
    image = read_png(source)
    alpha = image[:, :, 3] > 128
    keep = opened(alpha, 9)
    image = image.copy()
    image[~keep] = 0
    ys, xs = np.flatnonzero(keep.any(axis=1)), np.flatnonzero(keep.any(axis=0))
    image = image[ys[0]:ys[-1] + 1, xs[0]:xs[-1] + 1]
    h, w = image.shape[:2]
    side = int(max(h, w) * 1.08)
    square = np.zeros((side, side, 4), dtype=np.uint8)
    top, left = (side - h) // 2, (side - w) // 2
    square[top:top + h, left:left + w] = image
    premult = square.astype(np.float64)
    premult[:, :, :3] *= premult[:, :, 3:4] / 255
    OUT.mkdir(parents=True, exist_ok=True)
    previews = {}
    for size in SIZES + [256]:
        matrix = area_matrix(size, side)
        small = np.tensordot(np.tensordot(matrix, premult, axes=(1, 0)), matrix, axes=(1, 1)).transpose(0, 2, 1)
        alpha_small = small[:, :, 3:4]
        straight = np.where(alpha_small > 0, small[:, :, :3] * 255 / np.maximum(alpha_small, 1e-9), 0)
        rgba = np.concatenate([np.clip(straight, 0, 255), np.clip(alpha_small, 0, 255)], axis=2).round().astype(np.uint8)
        previews[size] = rgba
        if size in SIZES:
            (OUT / f"{size}.rgba").write_bytes(rgba.tobytes())
    if len(sys.argv) > 2:
        shot = previews[256].astype(np.float64)
        background = np.array([32, 34, 37], dtype=np.float64)
        flat = (shot[:, :, :3] * shot[:, :, 3:4] / 255 + background * (1 - shot[:, :, 3:4] / 255)).round().astype(np.uint8)
        raw = b"".join(b"\x00" + flat[y].tobytes() for y in range(256))

        def chunk(kind, body):
            return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body) & 0xFFFFFFFF)

        Path(sys.argv[2]).write_bytes(
            b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", 256, 256, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b"")
        )
    print("wrote", ", ".join(f"{size}.rgba" for size in SIZES))


main()
