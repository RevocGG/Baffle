#!/usr/bin/env python3
"""Generates assets/icon.png and a multi-size assets/icon.ico (no external deps).

Baffle mark: an exact-palette dark disc with teal spectrum bars.
"""
import zlib, struct, math, os

def draw(S):
    px = [[(0, 0, 0, 0) for _ in range(S)] for _ in range(S)]
    cx, cy, r = S / 2, S / 2, S / 2 - max(1.0, S * 2 / 128)
    aa = max(1.0, S * 1.5 / 128)
    k = S / 128.0

    for y in range(S):
        for x in range(S):
            d = math.hypot(x - cx, y - cy)
            if d <= r:
                edge = min(1.0, (r - d) / aa)
                # Exact Baffle app background; antialias only the alpha edge.
                px[y][x] = (0x0F, 0x11, 0x15, int(edge * 255))

    # Spectrum bars (teal), geometry proportional to size
    heights = [int(34 * k), int(58 * k), int(82 * k), int(58 * k), int(34 * k)]
    bw = max(2, int(12 * k))
    gap = max(2, int(8 * k))
    n = len(heights)
    # keep the bar block inside the disc, shrinking gap then bw if needed
    total = n * bw + (n - 1) * gap
    while total > S - 4 and gap > 1:
        gap -= 1
        total = n * bw + (n - 1) * gap
    while total > S - 4 and bw > 1:
        bw -= 1
        total = n * bw + (n - 1) * gap
    x0 = (S - total) // 2
    th = max(1, int(3 * k))  # rounded-cap threshold
    for i, h in enumerate(heights):
        bx = x0 + i * (bw + gap)
        by = (S - h) // 2
        for y in range(by, by + h):
            for x in range(bx, bx + bw):
                dx = min(x - bx, bx + bw - 1 - x)
                dy = min(y - by, by + h - 1 - y)
                if dx + dy >= th:
                    px[y][x] = (0x2D, 0xD4, 0xBF, 255)
    return px

def write_png(path, px):
    S = len(px)
    raw = b""
    for row in px:
        raw += b"\x00" + b"".join(struct.pack("4B", *p) for p in row)
    def chunk(tag, data):
        c = struct.pack(">I", len(data)) + tag + data
        return c + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", S, S, 8, 6, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(raw, 9))
    png += chunk(b"IEND", b"")
    with open(path, "wb") as f:
        f.write(png)
    return png

def write_ico(path, sizes):
    # ICO container with PNG-compressed frames (supported on Vista+)
    ico = struct.pack("<HHH", 0, 1, len(sizes))
    offset = 6 + 16 * len(sizes)
    for S, png in sizes:
        w = S if S < 256 else 0
        ico += struct.pack("<BBBBHHII", w, w, 0, 0, 1, 32, len(png), offset)
        offset += len(png)
    with open(path, "wb") as f:
        f.write(ico + b"".join(png for _, png in sizes))

if __name__ == "__main__":
    os.makedirs("assets", exist_ok=True)
    write_png("assets/icon.png", draw(128))
    sizes = [(S, write_png(os.path.join(os.environ.get("TEMP", "/tmp"), f"_ico{S}.png"), draw(S)))
             for S in (16, 24, 32, 48, 64, 128, 256)]
    # re-encode PNGs from the temp files so the ico holds self-contained frames
    frames = []
    for S in (16, 24, 32, 48, 64, 128, 256):
        frames.append((S, open(os.path.join(os.environ.get("TEMP", "/tmp"), f"_ico{S}.png"), "rb").read()))
    write_ico("assets/icon.ico", frames)
    for S in (16, 24, 32, 48, 64, 128, 256):
        os.remove(os.path.join(os.environ.get("TEMP", "/tmp"), f"_ico{S}.png"))
    print("assets/icon.png and assets/icon.ico written (7 sizes)")
