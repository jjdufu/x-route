#!/usr/bin/env python3
"""生成整页电路板背景（src/pcb.css）与电流走线表（src/pcb.js）。

两层，都是静态 SVG（data URI，浏览器只栅格化一次），红 / 绿各一份，状态切换只交叉淡入淡出不重绘：
- 整页底纹（.bg-red / .bg-green）：可无缝平铺的稀疏走线、过孔与贴片元件，铺满整个界面，透明度很低，不抢内容；
- 电源按钮周围（.st-red / .st-green）：中央由可点芯片 SVG 覆盖，底层保留四边引脚、每个引脚引出带 45° 折角的扇出走线、
  过孔、总线组与贴片元件，越靠近按钮越密；边缘由 CSS 径向遮罩淡出，与整页底纹融为一体（没有硬边）。
- 未启动 / 失败：红色，每条走线断开一小段（「断电」），完全静态；
- 运行中：绿色，走线接通；另由 pcb.js 列出的 8 段直线走线叠加电流脉冲（HTML 元素，只做 transform 动画），流向芯片 / 按钮。
运行：python3 scripts/gen-pcb.py（结果提交进仓库，构建时不需要 Python）。
"""
import math
import random
from pathlib import Path
from urllib.parse import quote

W, H = 480, 320
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
        cx, cy = cx * W / 400, cy * H / 252
        horiz = rnd.random() < 0.5
        w, h = (12, 6) if horiz else (6, 12)
        parts.append(f"<rect x='{fmt(cx - w / 2)}' y='{fmt(cy - h / 2)}' width='{w}' height='{h}' rx='1'/>")
        if horiz:
            parts.append(f"<rect x='{fmt(cx - w / 2 - 3)}' y='{fmt(cy - 3)}' width='3' height='6'/><rect x='{fmt(cx + w / 2)}' y='{fmt(cy - 3)}' width='3' height='6'/>")
        else:
            parts.append(f"<rect x='{fmt(cx - 3)}' y='{fmt(cy - h / 2 - 3)}' width='6' height='3'/><rect x='{fmt(cx - 3)}' y='{fmt(cy + h / 2)}' width='6' height='3'/>")
    g.append(f"<g fill='{color}' fill-opacity='.22'>" + "".join(parts) + "</g>")
    # 芯片本体改由首页可点 SVG 绘制；这里只保留引脚，与扇出走线对齐
    pr = []
    for (px, py), v, _, _ in pins():
        if v[0]:
            x = px if v[0] < 0 else px - PIN_LEN
            pr.append(f"<rect x='{fmt(x)}' y='{fmt(py - 2)}' width='{PIN_LEN}' height='4' rx='1'/>")
        else:
            y = py if v[1] < 0 else py - PIN_LEN
            pr.append(f"<rect x='{fmt(px - 2)}' y='{fmt(y)}' width='4' height='{PIN_LEN}' rx='1'/>")
    g.append(f"<g fill='{color}' fill-opacity='.5'>" + "".join(pr) + "</g>")
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


TILE = 240


def tile_svg(color, broken):
    """整页底纹：TILE 见方、可无缝平铺（走线不碰边），稀疏的折线走线 + 过孔 + 贴片元件"""
    r = random.Random(7)
    lines, vias, parts = [], [], []
    for _ in range(9):
        x, y = r.randrange(16, TILE - 16, 8), r.randrange(16, TILE - 16, 8)
        pts = [(x, y)]
        d = r.choice(((1, 0), (-1, 0), (0, 1), (0, -1)))
        for _seg in range(r.randint(2, 3)):
            n = r.randrange(16, 56, 8)
            nx, ny = pts[-1][0] + d[0] * n, pts[-1][1] + d[1] * n
            nx, ny = min(max(nx, 10), TILE - 10), min(max(ny, 10), TILE - 10)
            if (nx, ny) == pts[-1]:
                break
            pts.append((nx, ny))
            # 45° 折角后换一个正交方向
            k = r.randrange(8, 20, 4)
            t = (d[1], d[0]) if r.random() < 0.5 else (-d[1], -d[0])
            bx, by = nx + d[0] * k + t[0] * k, ny + d[1] * k + t[1] * k
            if 10 <= bx <= TILE - 10 and 10 <= by <= TILE - 10:
                pts.append((bx, by))
                d = t
        if len(pts) < 2:
            continue
        vias.append(pts[0])
        vias.append(pts[-1])
        if broken:
            lines += list(split_gap(pts, 6))
        else:
            lines.append(pts)
    for _ in range(4):
        cx, cy = r.randrange(20, TILE - 20), r.randrange(20, TILE - 20)
        if r.random() < 0.5:
            parts.append(f"<rect x='{cx - 6}' y='{cy - 3}' width='12' height='6' rx='1'/>")
        else:
            parts.append(f"<rect x='{cx - 3}' y='{cy - 6}' width='6' height='12' rx='1'/>")
    g = [f"<svg xmlns='http://www.w3.org/2000/svg' width='{TILE}' height='{TILE}' viewBox='0 0 {TILE} {TILE}'>"]
    g.append("<defs><pattern id='g' width='24' height='24' patternUnits='userSpaceOnUse'>"
             f"<path d='M24 0H0V24' fill='none' stroke='{color}' stroke-opacity='.035'/></pattern></defs>")
    g.append(f"<rect width='{TILE}' height='{TILE}' fill='url(%23g)'/>")
    g.append(f"<g fill='none' stroke='{color}' stroke-opacity='.12' stroke-width='1.2' stroke-linecap='round' stroke-linejoin='round'>")
    g.append("<path d='" + " ".join(path_d(t) for t in lines) + "'/></g>")
    g.append(f"<g fill='none' stroke='{color}' stroke-opacity='.18' stroke-width='1'>")
    g.append("".join(f"<circle cx='{fmt(x)}' cy='{fmt(y)}' r='2.2'/>" for x, y in vias))
    g.append("</g>")
    g.append(f"<g fill='{color}' fill-opacity='.10'>" + "".join(parts) + "</g>")
    g.append("</svg>")
    return "".join(g)


def bus_lines():
    bus = []
    for sx, sy in ((-1, -1), (1, -1), (-1, 1), (1, 1)):
        ox = 0 if sx < 0 else W
        oy = 0 if sy < 0 else H
        for k in range(6):
            y = oy - sy * (8 + k * 5)
            x_a = ox - sx * 2
            x_b = ox - sx * (28 + k * 5)
            x_c = x_b - sx * 10
            bus.append([(x_a, y), (x_b, y), (x_c, y - sy * 10), (x_c - sx * 22, y - sy * 10)])
    return bus


def ring_paths():
    """芯片周围同心圆角方环，营造参考图里的多层焊盘圈。"""
    out = []
    for i, pad in enumerate((18, 32, 48, 66)):
        x, y = X0 - pad, Y0 - pad
        w, h = CHIP + pad * 2, CHIP + pad * 2
        r = 16 + i * 2
        # 从靠近芯片上沿中点起笔，顺时针一圈，便于由内向外显现
        d = (f"M{fmt(CX)} {fmt(y)} "
             f"H{fmt(x + w - r)} A{r} {r} 0 0 1 {fmt(x + w)} {fmt(y + r)} "
             f"V{fmt(y + h - r)} A{r} {r} 0 0 1 {fmt(x + w - r)} {fmt(y + h)} "
             f"H{fmt(x + r)} A{r} {r} 0 0 1 {fmt(x)} {fmt(y + h - r)} "
             f"V{fmt(y + r)} A{r} {r} 0 0 1 {fmt(x + r)} {fmt(y)} Z")
        out.append({"d": d, "dist": 20 + i * 14, "kind": "ring"})
    return out


def beam_paths():
    """运行态左右能量束：穿过芯片中线的长水平线。"""
    out = []
    for dy, dist in ((0, 210), (-16, 230), (16, 230), (-32, 250), (32, 250)):
        y = CY + dy
        # 左、右两段，从芯片边向外
        out.append({"d": path_d([(X0 - 10, y), (8, y)]), "dist": dist, "kind": "beam"})
        out.append({"d": path_d([(X1 + 10, y), (W - 8, y)]), "dist": dist, "kind": "beam"})
    return out


def branch_traces():
    """额外曼哈顿分支，填满舞台中段，走线更密。"""
    r = random.Random(42)
    out = []
    for i in range(36):
        ang_q = i % 4
        v = [(-1, 0), (1, 0), (0, -1), (0, 1)][ang_q]
        f = (0, 1) if v[0] else (1, 0)
        # 从芯片外沿附近出发
        off = (i // 4 - 4) * 11
        if v[0]:
            p0 = (X0 - 12 if v[0] < 0 else X1 + 12, CY + off)
        else:
            p0 = (CX + off, Y0 - 12 if v[1] < 0 else Y1 + 12)
        s1 = 18 + (i % 5) * 10
        a = (p0[0] + v[0] * s1, p0[1] + v[1] * s1)
        turn = (12 + (i % 6) * 8) * (1 if i % 2 == 0 else -1)
        b = (a[0] + v[0] * abs(turn) * 0.3 + f[0] * turn, a[1] + v[1] * abs(turn) * 0.3 + f[1] * turn)
        c = to_edge(b, v)
        pts = [p0, a, b, c]
        out.append({"d": path_d(pts), "dist": round(math.hypot(p0[0] - CX, p0[1] - CY) + 20, 1), "kind": "branch"})
        # 折角过孔稍后再收集
        out[-1]["_via"] = (round(b[0], 1), round(b[1], 1))
    return out


def traces_export():
    """舞台可动画走线：路径从芯片引脚向外，供 stroke-dashoffset 由内向外修复。"""
    items = []
    for pin in pins():
        pts = trace(pin)
        px, py = pts[0]
        items.append({
            "d": path_d(pts),
            "dist": round(math.hypot(px - CX, py - CY), 1),
            "kind": "pin",
        })
    for pts in corner_traces():
        # corner_traces 是 [edge, chip]；改成从芯片向外
        pts = list(reversed(pts))
        px, py = pts[0]
        items.append({
            "d": path_d(pts),
            "dist": round(math.hypot(px - CX, py - CY), 1),
            "kind": "corner",
        })
    for pts in bus_lines():
        # 总线在角落，离芯片更远，较晚接通
        mid = pts[len(pts) // 2]
        items.append({
            "d": path_d(pts),
            "dist": round(math.hypot(mid[0] - CX, mid[1] - CY), 1),
            "kind": "bus",
        })
    items.extend(ring_paths())
    items.extend(beam_paths())
    vias = []
    for b in branch_traces():
        vx, vy = b.pop("_via")
        vias.append({"x": vx, "y": vy})
        items.append(b)
    for t in (trace(p) for p in pins()):
        if len(t) == 4:
            vias.append({"x": round(t[2][0], 1), "y": round(t[2][1], 1)})
        elif rnd.random() < 0.5:
            vias.append({"x": round(t[1][0], 1), "y": round(t[1][1], 1)})
    pr = []
    for (px, py), v, _, _ in pins():
        if v[0]:
            x = px if v[0] < 0 else px - PIN_LEN
            pr.append({"x": round(x, 1), "y": round(py - 2, 1), "w": PIN_LEN, "h": 4})
        else:
            y = py if v[1] < 0 else py - PIN_LEN
            pr.append({"x": round(px - 2, 1), "y": round(y, 1), "w": 4, "h": PIN_LEN})
    return items, vias, pr


def js_obj(d):
    parts = []
    for k, v in d.items():
        if isinstance(v, str):
            parts.append(f'{k}:"{v}"')
        else:
            parts.append(f"{k}:{v}")
    return "{" + ",".join(parts) + "}"


root = Path(__file__).resolve().parent.parent
red = data_uri(svg("%23ff6b5e", True))
green = data_uri(svg("%2357e389", False))
tred = data_uri(tile_svg("%23ff6b5e", True))
tgreen = data_uri(tile_svg("%2357e389", False))
css = f"""/* 由 scripts/gen-pcb.py 生成，勿手改。整页底纹 {TILE}x{TILE} 平铺，静态 SVG，只栅格化一次。
   舞台可动画走线在 pcb.js（PCB_TRACES），不再用 .st-layer 背景图。 */
.bg-red {{ background-color: #07080c; background-image: url("{tred}"); }}
.bg-green {{ background-color: #050a08; background-image: url("{tgreen}"); }}
.bg-layer {{ background-size: {TILE}px {TILE}px; background-repeat: repeat; }}
"""
(root / "src" / "pcb.css").write_text(css)
tr, vias, pins_out = traces_export()
flow_js = ",\n".join("  " + str(f).replace("'", '"') for f in flows())
flow_js = flow_js.replace('"x"', "x").replace('"y"', "y").replace('"len"', "len").replace('"deg"', "deg").replace('"delay"', "delay")
js = (
    f"// 由 scripts/gen-pcb.py 生成，勿手改。舞台坐标 {W}x{H}；走线从芯片向外，供 stroke-dashoffset 修复。\n"
    f"export const PCB_W = {W};\n"
    f"export const PCB_H = {H};\n"
    f"export const PCB_TRACES = [\n" + ",\n".join("  " + js_obj(x) for x in tr) + "\n];\n"
    f"export const PCB_VIAS = [\n" + ",\n".join("  " + js_obj(x) for x in vias) + "\n];\n"
    f"export const PCB_PINS = [\n" + ",\n".join("  " + js_obj(x) for x in pins_out) + "\n];\n"
    f"export const PCB_FLOW = [\n{flow_js}\n];\n"
)
(root / "src" / "pcb.js").write_text(js)
print("pcb.css", len(css), "bytes; traces", len(tr), "vias", len(vias), "pins", len(pins_out), "flows", len(flows()))
