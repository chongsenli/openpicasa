#!/usr/bin/env python3
"""从 packaging/logo.png 生成多尺寸应用图标（只用 Python 标准库，不依赖 Pillow）。

logo.png 是一张白底 1024×1024 的圆形徽标，直接当图标用有两个问题：
  1. 白底是方形的，放到深色停靠栏/面板上会变成一块白方块；
  2. 圆盘只占画布 70%，缩到 24px 时整体偏小，比同排其它图标"瘦一圈"。
所以这里的处理是：先按非白像素求出圆盘的外接框 → 按圆裁出 alpha（沿用原图已有的
抗锯齿边缘，再补 1px 覆盖渐变）→ 裁到圆盘外接正方形 + 少量留白 → 面积平均下采样。

面积平均下采样在**预乘 alpha**空间里做，否则透明边缘会出现一圈发黑的脏边。

用法：
  # Linux hicolor 主题
  python3 packaging/make_icons.py --logo packaging/logo.png --hicolor /tmp/icons

  # macOS .iconset（由 make_icon.py 调用，不在这里拼 icns）
  python3 packaging/make_icons.py --logo packaging/logo.png --iconset /tmp/OpenPicasa.iconset

  # 单个文件：给二进制内嵌的窗口图标用（crates/picasa-ui/assets/logo-256.png）
  python3 packaging/make_icons.py --logo packaging/logo.png --single packaging/logo-256.png
"""

import argparse
import os
import struct
import sys
import zlib

# 三通道同时 >= 该值即视为背景白（实测底噪是 253~254，纯白只有零星像素）
WHITE_CUTOFF = 250
# 圆盘外接框之外再留一点空，避免缩到小尺寸时边缘贴到画布边上
MARGIN = 0.03

# hicolor 主题要的尺寸（16/24 给面板和小列表，48 给启动器，256/512 给预览）
HICOLOR_SIZES = (16, 24, 32, 48, 64, 128, 256, 512)


# --------------------------------------------------------------------------- PNG 读写


def _paeth(a, b, c):
    p = a + b - c
    pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
    if pa <= pb and pa <= pc:
        return a
    if pb <= pc:
        return b
    return c


def read_png(path):
    """读 8bit 非隔行 PNG，返回 (w, h, RGBA bytearray)。"""
    data = open(path, "rb").read()
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError(f"{path} 不是 PNG")

    pos = 8
    idat = bytearray()
    ihdr = None
    while pos < len(data):
        length = struct.unpack(">I", data[pos:pos + 4])[0]
        tag = data[pos + 4:pos + 8]
        body = data[pos + 8:pos + 8 + length]
        if tag == b"IHDR":
            ihdr = struct.unpack(">IIBBBBB", body)
        elif tag == b"IDAT":
            idat += body
        elif tag == b"IEND":
            break
        pos += 12 + length

    if ihdr is None:
        raise ValueError(f"{path} 缺少 IHDR")
    w, h, depth, color_type, _, _, interlace = ihdr
    if depth != 8 or interlace != 0:
        raise ValueError(f"只支持 8bit 非隔行 PNG（当前 depth={depth} interlace={interlace}）")
    if color_type == 3:
        raise ValueError("不支持调色板 PNG，请先另存为 RGB 或 RGBA")
    channels = {0: 1, 2: 3, 4: 2, 6: 4}[color_type]

    raw = zlib.decompress(bytes(idat))
    stride = w * channels
    rgba = bytearray(w * h * 4)
    prev = bytearray(stride)
    pos = 0
    for y in range(h):
        ftype = raw[pos]
        pos += 1
        line = bytearray(raw[pos:pos + stride])
        pos += stride
        if ftype == 1:
            for x in range(channels, stride):
                line[x] = (line[x] + line[x - channels]) & 0xFF
        elif ftype == 2:
            for x in range(stride):
                line[x] = (line[x] + prev[x]) & 0xFF
        elif ftype == 3:
            for x in range(stride):
                a = line[x - channels] if x >= channels else 0
                line[x] = (line[x] + ((a + prev[x]) >> 1)) & 0xFF
        elif ftype == 4:
            for x in range(stride):
                a = line[x - channels] if x >= channels else 0
                line[x] = (line[x] + _paeth(a, prev[x], prev[x - channels] if x >= channels else 0)) & 0xFF
        elif ftype != 0:
            raise ValueError(f"未知的行过滤类型 {ftype}")

        obase = y * w * 4
        if channels == 4:
            rgba[obase:obase + w * 4] = line
        elif channels == 3:
            for x in range(w):
                s = x * 3
                d = obase + x * 4
                rgba[d] = line[s]
                rgba[d + 1] = line[s + 1]
                rgba[d + 2] = line[s + 2]
                rgba[d + 3] = 255
        elif channels == 2:
            for x in range(w):
                d = obase + x * 4
                g = line[x * 2]
                rgba[d] = rgba[d + 1] = rgba[d + 2] = g
                rgba[d + 3] = line[x * 2 + 1]
        else:  # 灰度
            for x in range(w):
                d = obase + x * 4
                g = line[x]
                rgba[d] = rgba[d + 1] = rgba[d + 2] = g
                rgba[d + 3] = 255
        prev = line
    return w, h, rgba


def write_png(path, width, height, pixels):
    """写 RGBA PNG（filter 0）；pixels 为 row-major 的 rgba 字节。"""
    raw = bytearray()
    stride = width * 4
    for y in range(height):
        raw.append(0)
        raw += pixels[y * stride:(y + 1) * stride]

    def chunk(tag, data):
        out = struct.pack(">I", len(data)) + tag + data
        return out + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)

    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(bytes(raw), 9))
    png += chunk(b"IEND", b"")
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    with open(path, "wb") as f:
        f.write(png)


# --------------------------------------------------------------------------- 裁圆


def non_white_bbox(pixels, w, h):
    """返回非白像素的外接框 (x0, y0, x1, y1)，含端点。"""
    x0, y0, x1, y1 = w, h, -1, -1
    for y in range(h):
        base = y * w * 4
        row_min = w
        row_max = -1
        for x in range(w):
            o = base + x * 4
            if pixels[o] < WHITE_CUTOFF or pixels[o + 1] < WHITE_CUTOFF or pixels[o + 2] < WHITE_CUTOFF:
                if x < row_min:
                    row_min = x
                row_max = x
        if row_max >= 0:
            if row_min < x0:
                x0 = row_min
            if row_max > x1:
                x1 = row_max
            if y < y0:
                y0 = y
            y1 = y
    if x1 < 0:
        raise ValueError("图像里找不到非白内容，是不是全白？")
    return x0, y0, x1, y1


def make_badge(pixels, w, h, margin=MARGIN):
    """裁到圆盘外接正方形并挖掉圆外像素，返回 (side, side, RGBA)。"""
    x0, y0, x1, y1 = non_white_bbox(pixels, w, h)
    cx = (x0 + x1 + 1) / 2.0
    cy = (y0 + y1 + 1) / 2.0
    radius = max(x1 - x0 + 1, y1 - y0 + 1) / 2.0
    side = int(round(radius * 2 * (1 + margin)))
    left = cx - side / 2.0
    top = cy - side / 2.0

    out = bytearray(side * side * 4)
    for dy in range(side):
        sy = top + dy + 0.5
        src_y = int(sy)
        if src_y < 0 or src_y >= h:
            continue
        obase = dy * side * 4
        for dx in range(side):
            sx = left + dx + 0.5
            # 到圆心的距离决定覆盖度：内侧满覆盖，边缘 1px 渐变到 0
            ddx = sx - cx
            ddy = sy - cy
            cov = radius + 0.5 - (ddx * ddx + ddy * ddy) ** 0.5
            if cov <= 0.0:
                continue
            src_x = int(sx)
            if src_x < 0 or src_x >= w:
                continue
            s = (src_y * w + src_x) * 4
            d = obase + dx * 4
            out[d] = pixels[s]
            out[d + 1] = pixels[s + 1]
            out[d + 2] = pixels[s + 2]
            out[d + 3] = 255 if cov >= 1.0 else int(255 * cov)
    return side, side, out


# --------------------------------------------------------------------------- 缩放


def _axis_weights(src, dst):
    """面积平均的重采样权重：返回 [(dst_index, [(src_index, weight), ...]), ...]。"""
    scale = src / dst
    table = []
    for d in range(dst):
        start = d * scale
        end = start + scale
        acc = []
        s = int(start)
        while s < end and s < src:
            overlap = min(end, s + 1) - max(start, s)
            if overlap > 1e-9:
                acc.append((s, overlap / scale))
            s += 1
        table.append(acc)
    return table


def resize(pixels, w, h, size):
    """面积平均下采样到 size×size（预乘 alpha，避免透明边缘发黑）。"""
    if w == size and h == size:
        return bytearray(pixels)

    xw = _axis_weights(w, size)
    # 水平 pass：h × size
    tmp = [None] * h
    for y in range(h):
        base = y * w * 4
        line = [0.0] * (size * 4)
        for dx in range(size):
            acc_r = acc_g = acc_b = acc_a = 0.0
            for sx, wt in xw[dx]:
                o = base + sx * 4
                a = pixels[o + 3] / 255.0
                if a > 0.0:
                    acc_r += pixels[o] * a * wt
                    acc_g += pixels[o + 1] * a * wt
                    acc_b += pixels[o + 2] * a * wt
                    acc_a += a * wt
            d = dx * 4
            line[d] = acc_r
            line[d + 1] = acc_g
            line[d + 2] = acc_b
            line[d + 3] = acc_a
        tmp[y] = line

    yw = _axis_weights(h, size)
    out = bytearray(size * size * 4)
    for dy in range(size):
        rows = yw[dy]
        for dx in range(size):
            d = dx * 4
            acc_r = acc_g = acc_b = acc_a = 0.0
            for sy, wt in rows:
                line = tmp[sy]
                acc_r += line[d] * wt
                acc_g += line[d + 1] * wt
                acc_b += line[d + 2] * wt
                acc_a += line[d + 3] * wt
            o = dy * size * 4 + d
            a = acc_a
            if a <= 1e-6:
                out[o:o + 4] = b"\x00\x00\x00\x00"
            else:
                out[o] = min(255, int(round(acc_r / a)))
                out[o + 1] = min(255, int(round(acc_g / a)))
                out[o + 2] = min(255, int(round(acc_b / a)))
                out[o + 3] = min(255, int(round(a * 255)))
    return out


class Logo:
    """读一次 logo.png，按需产出任意尺寸（中间结果复用，越大越先算）。"""

    def __init__(self, path):
        w, h, pixels = read_png(path)
        self.side, self.height, self.badge = make_badge(pixels, w, h)
        self._cache = {self.side: self.badge}

    def at(self, size):
        if size in self._cache:
            return self._cache[size]
        # 从"不比目标小太多"的已有尺寸缩，既省时又比一步缩到 16px 干净
        bigger = [s for s in self._cache if s > size]
        src = min(bigger) if bigger else self.side
        out = resize(self._cache[src], src, src, size)
        self._cache[size] = out
        return out


# --------------------------------------------------------------------------- 输出


def write_hicolor(logo, root, sizes=HICOLOR_SIZES, name="openpicasa"):
    for size in sorted(sizes, reverse=True):
        d = os.path.join(root, f"{size}x{size}", "apps")
        os.makedirs(d, exist_ok=True)
        path = os.path.join(d, f"{name}.png")
        write_png(path, size, size, logo.at(size))
        print(f"  {size}×{size} → {path}")


def write_iconset(logo, out_dir):
    """macOS .iconset 命名（icns 由 iconutil 在外层拼）。"""
    os.makedirs(out_dir, exist_ok=True)
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
    for size, name in specs:
        path = os.path.join(out_dir, name)
        write_png(path, size, size, logo.at(size))
        print(f"  {size}×{size} → {name}")


def main(argv=None):
    ap = argparse.ArgumentParser(description="从 logo.png 生成多尺寸应用图标")
    ap.add_argument("--logo", required=True, help="源图（建议 1024×1024 白底 PNG）")
    ap.add_argument("--hicolor", help="输出到 hicolor 主题根目录（会建 <size>x<size>/apps/）")
    ap.add_argument("--iconset", help="输出 macOS .iconset 目录")
    ap.add_argument("--single", help="输出单个 PNG（给二进制内嵌用），路径")
    ap.add_argument("--single-size", type=int, default=256, help="--single 的边长，默认 256（须为 4 的倍数）")
    ap.add_argument("--sizes", default=",".join(str(s) for s in HICOLOR_SIZES))
    ap.add_argument("--name", default="openpicasa", help="hicolor 里的文件名（不含 .png）")
    args = ap.parse_args(argv)

    if not (args.hicolor or args.iconset or args.single):
        ap.error("至少要给 --hicolor / --iconset / --single 之一")

    logo = Logo(args.logo)
    print(f"源图裁成 {logo.side}×{logo.side}（圆形遮罩 + {MARGIN:.0%} 留白）")

    if args.hicolor:
        sizes = [int(s) for s in args.sizes.split(",") if s.strip()]
        write_hicolor(logo, args.hicolor, sizes, args.name)
    if args.iconset:
        write_iconset(logo, args.iconset)
    if args.single:
        size = args.single_size
        if size % 4:
            ap.error("--single-size 必须是 4 的倍数（egui::IconData 的要求）")
        write_png(args.single, size, size, logo.at(size))
        print(f"  {size}×{size} → {args.single}")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (ValueError, OSError) as e:
        print(f"错误：{e}", file=sys.stderr)
        sys.exit(1)
