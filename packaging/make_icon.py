# 用 Python 标准库生成 OpenPicasa 的应用图标（不引入 Pillow）。
#
# 图标源：优先 packaging/logo.png（正式品牌图，与 Linux 侧共用同一份），
# 由 make_icons.py 裁圆 + 下采样；仓库里没有 logo.png 时才回退下面的自绘图
# （深色圆角底 + 白色「照片」符号，不参考原版 Picasa 图标，02 §8.3 的版权底线）。
#
# 用法：python3 packaging/make_icon.py <输出目录>
# 产出：<输出目录>/OpenPicasa.iconset/ + <输出目录>/OpenPicasa.icns（调用 iconutil）

import math
import os
import struct
import subprocess
import sys
import zlib

SS = 4  # 超采样倍数（抗锯齿）


def write_png(path, width, height, pixels):
    """pixels: bytearray of RGBA, row-major."""
    raw = bytearray()
    stride = width * 4
    for y in range(height):
        raw.append(0)  # filter type 0
        raw += pixels[y * stride:(y + 1) * stride]

    def chunk(tag, data):
        c = struct.pack(">I", len(data)) + tag + data
        c += struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
        return c

    ihdr = struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0)
    png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr)
    png += chunk(b"IDAT", zlib.compress(bytes(raw), 9))
    png += chunk(b"IEND", b"")
    with open(path, "wb") as f:
        f.write(png)


def lerp(a, b, t):
    return a + (b - a) * t


def in_round_rect(x, y, x0, y0, x1, y1, r):
    if x < x0 or x > x1 or y < y0 or y > y1:
        return False
    cx = min(max(x, x0 + r), x1 - r)
    cy = min(max(y, y0 + r), y1 - r)
    return (x - cx) ** 2 + (y - cy) ** 2 <= r * r


def render(size):
    """返回 size×size 的 RGBA 字节串。超采样后平均，得到抗锯齿边缘。"""
    W = size * SS
    buf = bytearray(W * W * 4)

    # 底：圆角方块（占 92%，留出 macOS 图标的安全边距）
    pad = W * 0.04
    x0, y0, x1, y1 = pad, pad, W - pad, W - pad
    radius = W * 0.22

    for y in range(W):
        for x in range(W):
            i = (y * W + x) * 4
            if not in_round_rect(x, y, x0, y0, x1, y1, radius):
                continue
            t = (y - y0) / max(1.0, (y1 - y0))
            # 深蓝 → 深青的竖向渐变（避免与原版橙红配色雷同）
            r = int(lerp(0x22, 0x0E, t))
            g = int(lerp(0x3A, 0x5A, t))
            b = int(lerp(0x6B, 0x7A, t))
            buf[i:i + 4] = bytes((r, g, b, 255))

    # 白色「照片」符号：一个圆角矩形 + 山 + 太阳
    fr_x0, fr_y0 = W * 0.22, W * 0.28
    fr_x1, fr_y1 = W * 0.78, W * 0.72
    fr_r = W * 0.04
    for y in range(int(fr_y0), int(fr_y1)):
        for x in range(int(fr_x0), int(fr_x1)):
            if not in_round_rect(x, y, fr_x0, fr_y0, fr_x1, fr_y1, fr_r):
                continue
            i = (y * W + x) * 4
            buf[i:i + 4] = bytes((0xFF, 0xFF, 0xFF, 255))

    # 太阳
    sun_cx, sun_cy, sun_r = W * 0.34, W * 0.38, W * 0.045
    for y in range(int(sun_cy - sun_r), int(sun_cy + sun_r) + 1):
        for x in range(int(sun_cx - sun_r), int(sun_cx + sun_r) + 1):
            if 0 <= x < W and 0 <= y < W:
                if (x - sun_cx) ** 2 + (y - sun_cy) ** 2 <= sun_r * sun_r:
                    i = (y * W + x) * 4
                    buf[i:i + 4] = bytes((0x2E, 0x6F, 0xB0, 255))

    # 山（两个三角形，用直线方程判定）
    def in_triangle(px, py, a, b, c):
        def sign(p1, p2, p3):
            return (p1[0] - p3[0]) * (p2[1] - p3[1]) - (p2[0] - p3[0]) * (p1[1] - p3[1])
        d1 = sign((px, py), a, b)
        d2 = sign((px, py), b, c)
        d3 = sign((px, py), c, a)
        neg = (d1 < 0) or (d2 < 0) or (d3 < 0)
        pos = (d1 > 0) or (d2 > 0) or (d3 > 0)
        return not (neg and pos)

    big = ((W * 0.26, W * 0.68), (W * 0.45, W * 0.44), (W * 0.64, W * 0.68))
    small = ((W * 0.52, W * 0.68), (W * 0.63, W * 0.54), (W * 0.74, W * 0.68))
    for y in range(int(fr_y0), int(fr_y1)):
        for x in range(int(fr_x0), int(fr_x1)):
            if not in_round_rect(x, y, fr_x0, fr_y0, fr_x1, fr_y1, fr_r):
                continue
            if in_triangle(x, y, *big) or in_triangle(x, y, *small):
                i = (y * W + x) * 4
                buf[i:i + 4] = bytes((0x2E, 0x6F, 0xB0, 255))

    # 下采样（盒式平均）
    out = bytearray(size * size * 4)
    for y in range(size):
        for x in range(size):
            rs = gs = bs = as_ = 0
            n = SS * SS
            for dy in range(SS):
                for dx in range(SS):
                    sx, sy = x * SS + dx, y * SS + dy
                    i = (sy * W + sx) * 4
                    rs += buf[i]
                    gs += buf[i + 1]
                    bs += buf[i + 2]
                    as_ += buf[i + 3]
            o = (y * size + x) * 4
            out[o:o + 4] = bytes((rs // n, gs // n, bs // n, as_ // n))
    return out


def main():
    out_dir = sys.argv[1] if len(sys.argv) > 1 else "dist"
    iconset = os.path.join(out_dir, "OpenPicasa.iconset")
    os.makedirs(iconset, exist_ok=True)

    # 优先用正式品牌图 packaging/logo.png（与 Linux 侧同一份源）；
    # 仓库里没有它时才退回下面的 procedural 自绘。
    logo = os.path.join(os.path.dirname(os.path.abspath(__file__)), "logo.png")
    if os.path.exists(logo):
        import make_icons  # 同目录，脚本运行时 sys.path[0] 就是 packaging/
        make_icons.write_iconset(make_icons.Logo(logo), iconset)
    else:
        print("未找到 packaging/logo.png，回退自绘图标", file=sys.stderr)
        # iconutil 要求的命名规则
        specs = [
            (16, "icon_16x16.png"),
            (32, "icon_16x16@2x.png"),
            (32, "icon_32x32.png"),
            (64, "icon_32x32@2x.png"),
            (128, "icon_128x128.png"),
            (256, "icon_128x128@2x.png"),
            (256, "icon_256x256.png"),
            (512, "icon_256x256@2x.png"),
            (512, "icon_512x512.png"),
            (1024, "icon_512x512@2x.png"),
        ]
        cache = {}
        for size, name in specs:
            if size not in cache:
                print(f"  渲染 {size}×{size} …", flush=True)
                cache[size] = render(size)
            write_png(os.path.join(iconset, name), size, size, cache[size])

    icns = os.path.join(out_dir, "OpenPicasa.icns")
    subprocess.run(["iconutil", "-c", "icns", iconset, "-o", icns], check=True)
    print(f"✅ {icns}")


if __name__ == "__main__":
    main()
