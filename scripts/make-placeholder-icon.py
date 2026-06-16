#!/usr/bin/env python3
"""Генерит 1024x1024 RGBA PNG: скруглённый квадрат с диагональным градиентом
на прозрачном фоне (форма в духе macOS-иконки). Только стандартная библиотека.
Заменяется реальной иконкой позже — просто положи свой icon-source.png."""
import zlib, struct, sys

W = H = 1024
RADIUS = 184            # скругление углов
MARGIN = 40             # поля до края канвы
# градиент: синий -> бирюзовый (не one-note, читаемо на свету и в тёмной теме)
C0 = (37, 99, 235)      # indigo-600
C1 = (13, 148, 136)     # teal-600


def rounded(x, y):
    """True, если пиксель внутри скруглённого квадрата."""
    lo, hi = MARGIN, W - 1 - MARGIN
    if x < lo or x > hi or y < lo or y > hi:
        return False
    dx = min(x - (lo + RADIUS), (hi - RADIUS) - x, 0)
    dy = min(y - (lo + RADIUS), (hi - RADIUS) - y, 0)
    return dx * dx + dy * dy <= RADIUS * RADIUS


def pixel(x, y):
    if not rounded(x, y):
        return (0, 0, 0, 0)
    t = (x + y) / (2 * (W - 1))           # 0..1 по диагонали
    r = round(C0[0] + (C1[0] - C0[0]) * t)
    g = round(C0[1] + (C1[1] - C0[1]) * t)
    b = round(C0[2] + (C1[2] - C0[2]) * t)
    return (r, g, b, 255)


def chunk(typ, data):
    body = typ + data
    return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xffffffff)


def main(path):
    raw = bytearray()
    for y in range(H):
        raw.append(0)                      # filter type 0 (None)
        for x in range(W):
            raw += bytes(pixel(x, y))
    sig = b"\x89PNG\r\n\x1a\n"
    ihdr = struct.pack(">IIBBBBB", W, H, 8, 6, 0, 0, 0)   # 8-bit RGBA
    data = sig + chunk(b"IHDR", ihdr) + chunk(b"IDAT", zlib.compress(bytes(raw), 9)) + chunk(b"IEND", b"")
    with open(path, "wb") as f:
        f.write(data)
    print(f"написал {path} ({len(data)} байт)")


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "src-tauri/icons/icon-source.png")
