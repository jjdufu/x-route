#!/usr/bin/env python3
"""生成首页舞台的电路板底图（src/pcb.css）与电流走线表（src/pcb.js）。

底图是静态 SVG（data URI，浏览器只栅格化一次）：细网格、中央芯片（四边各 9 个引脚）、
每个引脚引出一条带 45° 折角的走线（扇出到四边）、过孔 / 焊盘、角落的总线组与贴片元件。
- 未启动 / 失败：红色，每条走线在靠近芯片处断开一小段（「断电」），完全静态；
- 运行中：绿色，走线接通；另由 pcb.js 列出的 8 段直线走线叠加电流脉冲（HTML 元素，只做 transform 动画）。
运行：python3 scripts/gen-pcb.py（结果提交进仓库，构建时不需要 Python）。
"""
import math
import random
from pathlib import Path
from urllib.parse import quote

W, H = 400, 252
CX, CY = W / 2, H / 2
CHIP = 132                       # 芯片边长（电源按钮直径 120，压在芯片中央）
X0, Y0 = CX - CHIP / 2, CY - CHIP / 2
X1, Y1 = CX + CHIP / 2, CY + CHIP / 2
PIN_N, PIN_GAP, PIN_LEN = 9, 13, 8
FAN = 6                          # 每偏离中心一个引脚，折角处多扇出 6px
rnd = random.Random(20261111)


def pins():
    """每个引脚：(引脚外端点, 向外方向单位向量, 扇出方向单位向量, 偏移 o)"""
    out = []
    for i in range(PIN_N):
        o = i - PIN_N // 2
        d = o * PIN_GAP
        out.append(((X0 - PIN_LEN, CY + d), (-1, 0), (0, 1 if o > 0 else -1), o))   # 左
        out.append(((X1 + PIN_LEN, CY + d), (1, 0), (0, 1 if o > 0 else -1), o))    # 右
        out.append(((CX + d, Y0 - PIN_LEN), (0, -1), (1 if o > 0 else -1, 0), o))    # 上
        out.append(((CX + d, Y1 + PIN_LEN), (0, 1), (1 if o > 0 else -1, 0), o))     # 下
    return out


def to_edge(p, v):
    """从 p 沿 v 走到画布边缘（略出界，保证贴边）"""
    t = []
    if v[0]: t.append(((W + 2 if v[0] > 0 else -2) - p[0]) / v[0])
    if v[1]: t.append(((H + 2 if v[1] > 0 else -2) - p[1]) / v[1])
    k = min(t)
    return (p[0] + v[0] * k, p[1] + v[1] * k)


def trace(pin):
    (px, py), v, f, o = pin
    s = 8 + (PIN_N // 2 - abs(o)) * FAN          # 越靠外的引脚越早折，平行斜线不交叉
    a = (px + v[0] * s, py + v[1] * s)
    pts = [(px, py), a]
    if o:
        k = abs(o) * FAN
        b = (a[0] + v[0] * k + f[0] * k, a[1] + v[1] * k + f[1] * k)
        pts.append(b)
    pts.append(to_edge(pts[-1], v))
    return pts


def corner_traces():
    """芯片四角 45° 斜线直通画布角落方向（电流走线之一）"""
    out = []
    for sx, sy in ((-1, -1), (1, -1), (-1, 1), (1, 1)):
        c = (CX + sx * CHIP / 2, CY + sy * CHIP / 2)
        out.append([to_edge(c, (sx / math.sqrt(2), sy / math.sqrt(2))), c])
    return out


def fmt(n):
    return f"{n:.1f}".rstrip("0").rstrip(".")


def path_d(pts):
    return "M" + " L".join(f"{fmt(x)} {fmt(y)}" for x, y in pts)


def split_gap(pts, gap=7):
    """在第一段（引脚出来的那段）中间挖掉一小段，返回两条折线"""
    (ax, ay), (bx, by) = pts[0], pts[1]
    L = math.hypot(bx - ax, by - ay)
    if L < gap + 6:
        (ax, ay), (bx, by) = pts[-2], pts[-1]
        L = math.hypot(bx - ax, by - ay)
        i = len(pts) - 2
    else:
        i = 0
    t0 = rnd.uniform(0.25, 0.6)
    t1 = t0 + gap / L
    p0 = (ax + (bx - ax) * t0, ay + (by - ay) * t0)
    p1 = (ax + (bx - ax) * t1, ay + (by - ay) * t1)
    return pts[: i + 1] + [p0], [p1] + pts[i + 1:]


def svg(color, broken):
    traces = [trace(p) for p in pins()]
    corners = corner_traces()
    g = []
    g.append("<svg xmlns='http://www.w3.org/2000/svg' width='%d' height='%d' viewBox='0 0 %d %d'>" % (W, H, W, H))
    g.append("<defs><pattern id='g' width='16' height='16' patternUnits='userSpaceOnUse'>"
             f"<path d='M16 0H0V16' fill='none' stroke='{color}' stroke-opacity='.05'/></pattern></defs>")
    g.append(f"<rect width='{W}' height='{H}' fill='url(%23g)'/>")
    # 角落总线组：4 条平行线带一次 45° 斜移
    bus = []
    for sx, sy in ((-1, -1), (1, -1), (-1, 1), (1, 1)):
        ox = 0 if sx < 0 else W
        oy = 0 if sy < 0 else H
        for k in range(4):
            y = oy - sy * (8 + k * 5)
            x_a = ox - sx * 2
            x_b = ox - sx * (28 + k * 5)
            x_c = x_b - sx * 10
            bus.append([(x_a, y), (x_b, y), (x_c, y - sy * 10), (x_c - sx * 22, y - sy * 10)])
    lines = []
    if broken:
        for t in traces + corners:
            lines += split_gap(t)
        for b in bus:
            lines += split_gap(b, 5)
    else:
        lines = traces + corners + bus
    g.append(f"<g fill='none' stroke='{color}' stroke-opacity='.30' stroke-width='1.4' stroke-linecap='round' stroke-linejoin='round'>")
    g.append("<path d='" + " ".join(path_d(t) for t in lines) + "'/></g>")
    # 过孔：折角处与走线外端附近
    vias = []
    for t in traces:
        if len(t) == 4:
            vias.append(t[2])
        elif rnd.random() < 0.5:
            vias.append(t[1])
    g.append(f"<g fill='%23000' fill-opacity='.35' stroke='{color}' stroke-opacity='.55' stroke-width='1.2'>")
    g.append("".join(f"<circle cx='{fmt(x)}' cy='{fmt(y)}' r='2.6'/>" for x, y in vias))
    g.append("</g>")
    # 贴片元件（电阻 / 电容）：角落空地里的小焊盘对
    parts = []
    for cx, cy in ((54, 112), (346, 140), (58, 170), (342, 82), (110, 236), (290, 18), (300, 236), (100, 16)):
        horiz = rnd.random() < 0.5
        w, h = (12, 6) if horiz else (6, 12)
        parts.append(f"<rect x='{fmt(cx - w / 2)}' y='{fmt(cy - h / 2)}' width='{w}' height='{h}' rx='1'/>")
        if horiz:
            parts.append(f"<rect x='{fmt(cx - w / 2 - 3)}' y='{fmt(cy - 3)}' width='3' height='6'/><rect x='{fmt(cx + w / 2)}' y='{fmt(cy - 3)}' width='3' height='6'/>")
        else:
            parts.append(f"<rect x='{fmt(cx - 3)}' y='{fmt(cy - h / 2 - 3)}' width='6' height='3'/><rect x='{fmt(cx - 3)}' y='{fmt(cy + h / 2)}' width='6' height='3'/>")
    g.append(f"<g fill='{color}' fill-opacity='.22'>" + "".join(parts) + "</g>")
    # 芯片：外框、内框、引脚、角标
    g.append(f"<rect x='{fmt(X0)}' y='{fmt(Y0)}' width='{CHIP}' height='{CHIP}' rx='12' fill='%23000' fill-opacity='.45' "
             f"stroke='{color}' stroke-opacity='.55' stroke-width='1.5'/>")
    g.append(f"<rect x='{fmt(X0 + 8)}' y='{fmt(Y0 + 8)}' width='{CHIP - 16}' height='{CHIP - 16}' rx='8' fill='none' "
             f"stroke='{color}' stroke-opacity='.22' stroke-dasharray='2 4'/>")
    pr = []
    for (px, py), v, _, _ in pins():
        if v[0]:
            x = px if v[0] < 0 else px - PIN_LEN
            pr.append(f"<rect x='{fmt(x)}' y='{fmt(py - 2)}' width='{PIN_LEN}' height='4' rx='1'/>")
        else:
            y = py if v[1] < 0 else py - PIN_LEN
            pr.append(f"<rect x='{fmt(px - 2)}' y='{fmt(y)}' width='4' height='{PIN_LEN}' rx='1'/>")
    g.append(f"<g fill='{color}' fill-opacity='.5'>" + "".join(pr) + "</g>")
    g.append(f"<circle cx='{fmt(X0 + 14)}' cy='{fmt(Y0 + 14)}' r='3' fill='{color}' fill-opacity='.5'/>")
    g.append("</svg>")
    return "".join(g)


def data_uri(s):
    return "data:image/svg+xml," + quote(s, safe="/:=' .,-%#()")


def flows():
    """电流走线：四边中间引脚的直线 + 四角斜线，方向从外向芯片"""
    segs = []
    tr = [trace(p) for p in pins()]
    for t, p in zip(tr, pins()):
        if p[3] == 0:
            segs.append((t[-1], t[0]))
    for c in corner_traces():
        segs.append((c[0], c[1]))
    out = []
    for i, ((ax, ay), (bx, by)) in enumerate(segs):
        L = math.hypot(bx - ax, by - ay)
        ang = math.degrees(math.atan2(by - ay, bx - ax))
        out.append({"x": round(ax, 1), "y": round(ay, 1), "len": round(L, 1), "deg": round(ang, 1), "delay": round((i * 0.37) % 1.6, 2)})
    return out


root = Path(__file__).resolve().parent.parent
red = data_uri(svg("%23ff6b5e", True))
green = data_uri(svg("%2357e389", False))
css = f"""/* 由 scripts/gen-pcb.py 生成，勿手改。首页舞台电路板底图：{W}x{H} 静态 SVG，居中，只栅格化一次 */
.stage {{
  background-color: #140d0e;
  background-image: radial-gradient(ellipse at 50% 50%, transparent 0 40%, rgba(10, 10, 12, .78) 86%), url("{red}");
  background-size: 100% 100%, {W}px {H}px;
  background-position: center, center;
  background-repeat: no-repeat;
}}
.stage.running {{
  background-color: #0a120d;
  background-image: radial-gradient(ellipse at 50% 50%, transparent 0 40%, rgba(8, 12, 10, .72) 86%), url("{green}");
}}
"""
(root / "src" / "pcb.css").write_text(css)
js = "// 由 scripts/gen-pcb.py 生成，勿手改。电流走线（舞台坐标 %dx%d，起点在外、终点在芯片）\nexport const PCB_W = %d;\nexport const PCB_H = %d;\nexport const PCB_FLOW = [\n%s\n];\n" % (
    W, H, W, H, ",\n".join("  " + str(f).replace("'", '"') for f in flows()))
js = js.replace('"x"', "x").replace('"y"', "y").replace('"len"', "len").replace('"deg"', "deg").replace('"delay"', "delay")
(root / "src" / "pcb.js").write_text(js)
print("pcb.css", len(css), "bytes; flows", len(flows()))
