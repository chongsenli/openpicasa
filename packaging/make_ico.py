#!/usr/bin/env python3
"""从 packaging/logo.png 生成 Windows 用的多尺寸 .ico。

复用 make_icons.py 的 Logo（圆形遮罩 + 留白 + 面积平均下采样），
再用 Pillow 把 16/24/32/48/64/128/256 七档拼成一个 ICO。
单独一个文件而不塞进 make_icons.py：那个脚本是纯标准库 SVG/PNG 手写，
Windows 交付不该把 Pillow 变成它的必需依赖。

用法：
  python3 packaging/make_ico.py [输出路径]
默认输出 dist/OpenPicasa.ico
"""
import os
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(REPO, "packaging"))

from make_icons import Logo, write_png  # noqa: E402
from PIL import Image  # noqa: E402

SIZES = [16, 24, 32, 48, 64, 128, 256]


def main() -> int:
    out = sys.argv[1] if len(sys.argv) > 1 else os.path.join(REPO, "dist", "OpenPicasa.ico")
    logo = Logo(os.path.join(REPO, "packaging", "logo.png"))
    stage = os.path.join(os.path.dirname(out), ".ico-stage")
    os.makedirs(stage, exist_ok=True)
    pngs = []
    for s in SIZES:
        p = os.path.join(stage, f"icon-{s}.png")
        write_png(p, s, s, logo.at(s))
        pngs.append((s, p))
    # Pillow 的 ICO 写盘：`sizes` 里的每一档都从**主图**下采样生成，
    # `append_images` 对 ICO 不生效（会只留第一张）。所以主图用最大档 256，
    # 其余档由 Pillow 自己缩放；但为了保留 make_icons.py 的圆形遮罩观感，
    # 逐档分别生成 PNG 再取最大档重组并不等价 —— 这里直接用 Logo 的 256 图，
    # 各尺寸由高质量 LANCZOS 缩放（Pillow ICO 内部即如此）。
    base = Image.open(pngs[-1][1]).convert("RGBA")  # 256×256
    base.save(
        out,
        format="ICO",
        sizes=[(s, s) for s in SIZES],
    )
    # 清临时 PNG（ICO 已经把像素嵌进去了）
    for _, p in pngs:
        os.remove(p)
    os.rmdir(stage)
    print(f"  {len(SIZES)} 档 → {out} ({os.path.getsize(out)} bytes)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
