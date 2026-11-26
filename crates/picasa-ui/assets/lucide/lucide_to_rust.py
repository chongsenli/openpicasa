import json, math, re, sys

import os

HERE = os.path.dirname(os.path.abspath(__file__))
RAW = os.path.join(HERE, 'lucide-0.544.0-raw.json')
OUT = os.path.join(HERE, '..', '..', 'src', 'icons_lucide.rs')

raw = json.load(open(RAW))
V = 24.0
CURVE_STEPS = 14
ARC_STEPS = 14
CIRCLE_STEPS = 32
CORNER_STEPS = 8

def parse_d(d):
    """返回若干子路径，每项 (points, closed)。处理 M/L/H/V/C/S/Q/T/A/Z。"""
    toks = re.findall(r'[MmLlHhVvCcSsQqTtAaZz]|-?\d*\.?\d+(?:[eE][-+]?\d+)?', d)
    i = 0
    subs, pts, closed = [], [], False
    cx = cy = 0.0
    start = (0.0, 0.0)
    cmd = None
    def flush():
        nonlocal pts, closed
        if pts:
            # Lucide 有些图标几何上首尾相接却没写 Z（如 heart 用圆弧收口），
            # 按首尾点距离补判闭合，否则实心绘制会漏填充。
            if not closed and len(pts) > 2:
                (x0, y0), (x1, y1) = pts[0], pts[-1]
                if (x0 - x1) ** 2 + (y0 - y1) ** 2 < 1e-4:
                    closed = True
            subs.append((pts, closed))
        pts, closed = [], False
    while i < len(toks):
        t = toks[i]
        if re.match(r'[A-Za-z]', t):
            cmd = t; i += 1
            if cmd in 'Zz':
                closed = True
                flush()
                cx, cy = start
                continue
        def n():
            nonlocal i
            v = float(toks[i]); i += 1; return v
        if cmd in 'Mm':
            x, y = n(), n()
            if cmd == 'm': x, y = cx + x, cy + y
            flush()
            cx, cy = x, y; start = (x, y)
            pts = [(x, y)]
            cmd = 'L' if cmd == 'M' else 'l'
        elif cmd in 'Ll':
            x, y = n(), n()
            if cmd == 'l': x, y = cx + x, cy + y
            pts.append((x, y)); cx, cy = x, y
        elif cmd in 'Hh':
            x = n()
            if cmd == 'h': x = cx + x
            pts.append((x, cy)); cx = x
        elif cmd in 'Vv':
            y = n()
            if cmd == 'v': y = cy + y
            pts.append((cx, y)); cy = y
        elif cmd in 'CcSs':
            if cmd in 'Cc':
                x1, y1, x2, y2, x, y = n(), n(), n(), n(), n(), n()
                if cmd == 'c':
                    x1, y1, x2, y2, x, y = cx+x1, cy+y1, cx+x2, cy+y2, cx+x, cy+y
                ccx, ccy = x1, y1
            else:
                x2, y2, x, y = n(), n(), n(), n()
                if cmd == 's':
                    x2, y2, x, y = cx+x2, cy+y2, cx+x, cy+y
                ccx, ccy = cx, cy
            for k in range(1, CURVE_STEPS + 1):
                u = k / CURVE_STEPS
                mx = (1-u)**3*cx + 3*(1-u)**2*u*ccx + 3*(1-u)*u*u*x2 + u**3*x
                my = (1-u)**3*cy + 3*(1-u)**2*u*ccy + 3*(1-u)*u*u*y2 + u**3*y
                pts.append((mx, my))
            cx, cy = x, y
        elif cmd in 'QqTt':
            if cmd in 'Qq':
                x1, y1, x, y = n(), n(), n(), n()
                if cmd == 'q': x1, y1, x, y = cx+x1, cy+y1, cx+x, cy+y
            else:
                x, y = n(), n()
                if cmd == 't': x, y = cx+x, cy+y
                x1, y1 = cx, cy
            for k in range(1, CURVE_STEPS + 1):
                u = k / CURVE_STEPS
                mx = (1-u)**2*cx + 2*(1-u)*u*x1 + u*u*x
                my = (1-u)**2*cy + 2*(1-u)*u*y1 + u*u*y
                pts.append((mx, my))
            cx, cy = x, y
        elif cmd in 'Aa':
            rx, ry, rot, laf, sf, x, y = n(), n(), n(), n(), n(), n(), n()
            if cmd == 'a': x, y = cx + x, cy + y
            pts.extend(arc_points(cx, cy, rx, ry, rot, laf, sf, x, y))
            cx, cy = x, y
        else:
            i += 1
    flush()
    return subs

def arc_points(x0, y0, rx, ry, rot_deg, laf, sf, x1, y1):
    """SVG 椭圆弧 → 折线（标准端点参数化）。"""
    if rx == 0 or ry == 0:
        return [(x1, y1)]
    phi = math.radians(rot_deg)
    cos_p, sin_p = math.cos(phi), math.sin(phi)
    dx2, dy2 = (x0 - x1) / 2.0, (y0 - y1) / 2.0
    x1p = cos_p*dx2 + sin_p*dy2
    y1p = -sin_p*dx2 + cos_p*dy2
    rx, ry = abs(rx), abs(ry)
    lam = (x1p**2)/(rx**2) + (y1p**2)/(ry**2)
    if lam > 1:
        s = math.sqrt(lam); rx *= s; ry *= s
    num = rx**2*ry**2 - rx**2*y1p**2 - ry**2*x1p**2
    den = rx**2*y1p**2 + ry**2*x1p**2
    co = math.sqrt(max(0.0, num/den)) if den else 0.0
    if laf == sf: co = -co
    cxp = co * (rx*y1p/ry); cyp = co * (-ry*x1p/rx)
    cx = cos_p*cxp - sin_p*cyp + (x0+x1)/2
    cy = sin_p*cxp + cos_p*cyp + (y0+y1)/2
    def ang(ux, uy, vx, vy):
        d = (ux*vx + uy*vy) / (math.hypot(ux,uy)*math.hypot(vx,vy))
        d = max(-1.0, min(1.0, d))
        a = math.acos(d)
        if ux*vy - uy*vx < 0: a = -a
        return a
    th1 = ang(1, 0, (x1p-cxp)/rx, (y1p-cyp)/ry)
    dth = ang((x1p-cxp)/rx, (y1p-cyp)/ry, (-x1p-cxp)/rx, (-y1p-cyp)/ry)
    if sf == 0 and dth > 0: dth -= 2*math.pi
    if sf == 1 and dth < 0: dth += 2*math.pi
    out = []
    steps = max(4, ARC_STEPS)
    for k in range(1, steps + 1):
        th = th1 + dth*k/steps
        ex = cx + rx*math.cos(th)*cos_p - ry*math.sin(th)*sin_p
        ey = cy + rx*math.cos(th)*sin_p + ry*math.sin(th)*cos_p
        out.append((ex, ey))
    return out

def circle_subs(cx, cy, r):
    pts = [(cx + r*math.cos(2*math.pi*k/CIRCLE_STEPS), cy + r*math.sin(2*math.pi*k/CIRCLE_STEPS))
           for k in range(CIRCLE_STEPS)]
    return [([(cx + r, cy)] + pts[1:], True)] if False else [(pts, True)]

def rect_subs(x, y, w, h, rx, ry):
    rx = min(rx, w/2); ry = min(ry, h/2)
    if rx <= 0.01 or ry <= 0.01:
        return [([(x,y),(x+w,y),(x+w,y+h),(x,y+h)], True)]
    pts = []
    def corner(cx, cy, a0, a1):
        for k in range(CORNER_STEPS + 1):
            a = a0 + (a1-a0)*k/CORNER_STEPS
            pts.append((cx + rx*math.cos(a), cy + ry*math.sin(a)))
    corner(x+w-rx, y+ry, -math.pi/2, 0)
    corner(x+w-rx, y+h-ry, 0, math.pi/2)
    corner(x+rx, y+h-ry, math.pi/2, math.pi)
    corner(x+rx, y+ry, math.pi, 1.5*math.pi)
    return [(pts, True)]

def num(attrs, k, d=0.0):
    m = re.search(rf'\b{k}="([-\d.]+)"', attrs)
    return float(m.group(1)) if m else d

def svg_to_subs(svg):
    body = re.search(r'<svg[^>]*>([\s\S]*)</svg>', svg).group(1)
    subs = []
    for m in re.finditer(r'<(path|rect|circle|line|polyline|polygon|ellipse)\b([^>]*)/?>', body):
        tag, a = m.group(1), m.group(2)
        if tag == 'path':
            d = re.search(r'\bd="([^"]+)"', a)
            if d: subs.extend(parse_d(d.group(1)))
        elif tag == 'rect':
            subs.extend(rect_subs(num(a,'x'), num(a,'y'), num(a,'width'), num(a,'height'),
                                  num(a,'rx', num(a,'ry')), num(a,'ry', num(a,'rx'))))
        elif tag == 'circle':
            subs.extend(circle_subs(num(a,'cx'), num(a,'cy'), num(a,'r')))
        elif tag == 'ellipse':
            subs.extend(circle_subs(num(a,'cx'), num(a,'cy'), num(a,'rx')))
        elif tag == 'line':
            subs.append(([(num(a,'x1'), num(a,'y1')), (num(a,'x2'), num(a,'y2'))], False))
        elif tag in ('polyline','polygon'):
            p = re.search(r'\bpoints="([^"]+)"', a)
            if p:
                v = [float(x) for x in re.findall(r'-?\d*\.?\d+', p.group(1))]
                pts = list(zip(v[0::2], v[1::2]))
                subs.append((pts, tag == 'polygon'))
    return subs

out = ["//! Lucide 图标路径数据（ISC 许可，https://lucide.dev）。",
       "//!",
       "//! **自动生成，不要手改**：由 `assets/icons/lucide_to_rust.py` 从 `lucide-static@0.544.0`",
       "//! 的 SVG 烘焙而来（24×24 viewBox，归一化到 0..1，曲线按 14 段折线展开）。",
       "//! 重新生成见 `docs/M5-V1-开发分支待办.md` 的「图标换 Lucide」小节。",
       "",
       "#![allow(clippy::approx_constant)] // 纯坐标数据，偶然接近 π 的分数值，不是常量",
       "",
       "/// 一条子路径：归一化点列（0..1），`closed` 表示首尾相接。",
       "#[derive(Clone, Copy, Debug)]",
       "pub struct SubPath {",
       "    pub pts: &'static [(f32, f32)],",
       "    pub closed: bool,",
       "}",
       ""]
names = []
total = 0
for slot, info in raw.items():
    subs = svg_to_subs(info['svg'])
    if not subs:
        print('EMPTY', slot, file=sys.stderr); continue
    const = re.sub(r'(?<!^)(?=[A-Z])', '_', slot).upper()
    names.append((slot, const))
    out.append(f"/// Lucide `{info['name']}`")
    out.append(f"pub static {const}: &[SubPath] = &[")
    for pts, closed in subs:
        norm = [(round(x/V, 4), round(y/V, 4)) for x, y in pts]
        # 去掉连续重复点
        ded = [norm[0]]
        for p in norm[1:]:
            if abs(p[0]-ded[-1][0]) > 1e-4 or abs(p[1]-ded[-1][1]) > 1e-4:
                ded.append(p)
        if len(ded) < 2: continue
        total += len(ded)
        body = ", ".join(f"({x:g}, {y:g})" for x, y in ded)
        out.append(f"    SubPath {{ pts: &[{body}], closed: {str(closed).lower()} }},")
    out.append("];")
    out.append("")
open(OUT,'w').write("\n".join(out))
print(f"slots={len(names)} points={total} bytes={len(chr(10).join(out))}")
