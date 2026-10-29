#!/usr/bin/env python3
"""PNG 像素探针：只用标准库解码，用于**数值化**核对界面截图。

为什么需要它：截图会被各种工具缩放，靠肉眼（或视觉模型）估坐标经常差几十像素，
把「提示框在底部」误判成「在中间」。这里直接读原始像素，输出「某个颜色的连通区域包围盒」，
断言就能写成数字。

用法：
  python3 tools/pngprobe.py shot.png --dark          # 找深色块（toast/横幅）
  python3 tools/pngprobe.py shot.png --rect 0,0,100,50  # 输出该区域的主色
"""

import sys
import zlib
import struct


def read_png(path):
    with open(path, "rb") as f:
        data = f.read()
    assert data[:8] == b"\x89PNG\r\n\x1a\n", "不是 PNG"
    pos = 8
    idat = bytearray()
    w = h = depth = color = None
    while pos < len(data):
        (length,) = struct.unpack(">I", data[pos : pos + 4])
        ctype = data[pos + 4 : pos + 8]
        chunk = data[pos + 8 : pos + 8 + length]
        pos += 12 + length
        if ctype == b"IHDR":
            w, h, depth, color, comp, filt, inter = struct.unpack(">IIBBBBB", chunk)
            assert depth == 8 and inter == 0, "只支持 8bit 非隔行"
            assert color in (2, 6), "只支持 RGB/RGBA"
        elif ctype == b"IDAT":
            idat += chunk
        elif ctype == b"IEND":
            break
    raw = zlib.decompress(bytes(idat))
    bpp = 4 if color == 6 else 3
    stride = w * bpp
    out = bytearray(w * h * bpp)
    prev = bytearray(stride)
    p = 0
    for y in range(h):
        ft = raw[p]
        p += 1
        line = bytearray(raw[p : p + stride])
        p += stride
        if ft == 0:
            pass
        elif ft == 1:  # Sub
            for i in range(bpp, stride):
                line[i] = (line[i] + line[i - bpp]) & 0xFF
        elif ft == 2:  # Up
            for i in range(stride):
                line[i] = (line[i] + prev[i]) & 0xFF
        elif ft == 3:  # Average
            for i in range(stride):
                a = line[i - bpp] if i >= bpp else 0
                line[i] = (line[i] + ((a + prev[i]) >> 1)) & 0xFF
        elif ft == 4:  # Paeth
            for i in range(stride):
                a = line[i - bpp] if i >= bpp else 0
                b = prev[i]
                c = prev[i - bpp] if i >= bpp else 0
                pa, pb, pc = abs(b - c), abs(a - c), abs(a + b - 2 * c)
                pr = a if (pa <= pb and pa <= pc) else (b if pb <= pc else c)
                line[i] = (line[i] + pr) & 0xFF
        else:
            raise ValueError(f"未知过滤类型 {ft}")
        out[y * stride : (y + 1) * stride] = line
        prev = line
    return w, h, bpp, out


def is_dark(px, r, g, b, thresh=90):
    return px[0] < thresh and px[1] < thresh and px[2] < thresh


def main():
    path = sys.argv[1]
    w, h, bpp, buf = read_png(path)
    print(f"尺寸：{w}x{h} 通道={bpp}")

    if "--dark" in sys.argv:
        # 找「连续 >=60 像素的深色行段」→ 汇总 bbox
        minx, miny, maxx, maxy = w, h, -1, -1
        for y in range(h):
            run = 0
            for x in range(w):
                i = (y * w + x) * bpp
                if is_dark(buf[i : i + 3], 0, 0, 0):
                    run += 1
                    if run >= 60:
                        minx = min(minx, x - run + 1)
                        maxx = max(maxx, x)
                        miny = min(miny, y)
                        maxy = max(maxy, y)
                else:
                    run = 0
        if maxx < 0:
            print("没有找到深色块")
        else:
            print(f"深色块 bbox: x {minx}..{maxx}  y {miny}..{maxy}"
                  f"  (相对高度 {miny/h*100:.1f}% .. {maxy/h*100:.1f}%)")

    if "--row" in sys.argv:
        # 沿一行扫描「同色连续段」，用来量左栏宽度、栅格起点、单元格间距这些硬边界
        y = int(sys.argv[sys.argv.index("--row") + 1])
        scale = float(sys.argv[sys.argv.index("--scale") + 1]) if "--scale" in sys.argv else 1.0
        if 0 <= y < h:
            runs = []
            start = 0
            cur = (buf[y * w * bpp], buf[y * w * bpp + 1], buf[y * w * bpp + 2])
            for x in range(1, w):
                i = (y * w + x) * bpp
                c = (buf[i], buf[i + 1], buf[i + 2])
                if c != cur:
                    if x - start >= 4:
                        runs.append((start, x - 1, cur))
                    start, cur = x, c
            if w - start >= 4:
                runs.append((start, w - 1, cur))
            print(f"第 {y} 行共有 {len(runs)} 段（坐标已除以 {scale}）")
            for a, b, c in runs[:40]:
                print(
                    f"  {a/scale:7.1f} .. {b/scale:7.1f}  "
                    f"宽 {(b-a+1)/scale:6.1f}  #{c[0]:02X}{c[1]:02X}{c[2]:02X}"
                )

    if "--rect" in sys.argv:
        spec = sys.argv[sys.argv.index("--rect") + 1]
        x0, y0, x1, y1 = (int(v) for v in spec.split(","))
        hist = {}
        for y in range(y0, min(y1, h)):
            for x in range(x0, min(x1, w)):
                i = (y * w + x) * bpp
                key = (buf[i], buf[i + 1], buf[i + 2])
                hist[key] = hist.get(key, 0) + 1
        for color, n in sorted(hist.items(), key=lambda kv: -kv[1])[:5]:
            print(f"  #{color[0]:02X}{color[1]:02X}{color[2]:02X}  {n} px")


if __name__ == "__main__":
    main()
