// 可随容器尺寸重绘的切角 HUD 边框：外细线 + 转角亮折角托架 + 内细线。
const NS = "http://www.w3.org/2000/svg";

const TONES = {
  cyan:    { dim: "rgba(0, 196, 216, .42)", bright: "#00d8ee", glow: "rgba(0, 216, 238, .45)" },
  green:   { dim: "rgba(57, 255, 136, .40)", bright: "#3dff8a", glow: "rgba(61, 255, 138, .55)" },
  magenta: { dim: "rgba(255, 43, 214, .40)", bright: "#ff2bd6", glow: "rgba(255, 43, 214, .45)" },
};

function el(name, attrs = {}, kids = []) {
  const n = document.createElementNS(NS, name);
  for (const [k, v] of Object.entries(attrs)) if (v != null) n.setAttribute(k, String(v));
  for (const c of kids) n.append(c);
  return n;
}
const f = (n) => (Math.round(n * 10) / 10).toFixed(1).replace(/\.0$/, "");

function chamferPts(w, h, c, pad) {
  // 内缩 pad 时切角轴向长度取 c - pad*(2-√2)，三线保持平行
  const x0 = pad, y0 = pad, x1 = w - pad, y1 = h - pad;
  const k = Math.max(2, c - pad * (2 - Math.SQRT2));
  return [
    [x0 + k, y0], [x1 - k, y0], [x1, y0 + k], [x1, y1 - k],
    [x1 - k, y1], [x0 + k, y1], [x0, y1 - k], [x0, y0 + k],
  ];
}
function poly(pts, close = true) {
  return "M" + pts.map(([x, y]) => `${f(x)} ${f(y)}`).join("L") + (close ? "Z" : "");
}

// 四角亮托架：沿外沿走一段、45° 折过切角、再沿邻边走一段。
function brackets(w, h, c, pad, arm) {
  const x0 = pad, y0 = pad, x1 = w - pad, y1 = h - pad;
  const k = Math.max(2, c - pad * (2 - Math.SQRT2));
  const a = Math.max(8, arm);
  return [
    // TL
    [[x0 + k + a, y0], [x0 + k, y0], [x0, y0 + k], [x0, y0 + k + a]],
    // TR
    [[x1 - k - a, y0], [x1 - k, y0], [x1, y0 + k], [x1, y0 + k + a]],
    // BR
    [[x1, y1 - k - a], [x1, y1 - k], [x1 - k, y1], [x1 - k - a, y1]],
    // BL
    [[x0, y1 - k - a], [x0, y1 - k], [x0 + k, y1], [x0 + k + a, y1]],
  ].map((pts) => poly(pts, false));
}

// 卡片底边内线：中段梯形台阶 + 右侧断开，给十六进制编号留空。
function cardInner(w, h, c, pad) {
  const x0 = pad, y0 = pad, x1 = w - pad, y1 = h - pad;
  const k = Math.max(2, c - pad * (2 - Math.SQRT2));
  const stepW = Math.max(22, Math.min(40, w * 0.13));
  const stepH = 7;
  const mid = w * 0.42;
  const s0 = mid - stepW / 2, s1 = mid + stepW / 2;
  // 右下十六进制编号缺口（与 .neo-hex 对齐）
  const hexR = x1 - 8, hexL = hexR - 52;
  const top = poly([
    [x0, y0 + k], [x0 + k, y0], [x1 - k, y0], [x1, y0 + k], [x1, y1 - k],
  ], false);
  const leftAndStep = poly([
    [x0, y0 + k], [x0, y1 - k], [x0 + k, y1],
    [s0 - stepH, y1], [s0, y1 - stepH], [s1, y1 - stepH], [s1 + stepH, y1],
    [hexL - 28, y1],
  ], false);
  const shortA = poly([[hexL - 20, y1], [hexL - 10, y1]], false);
  const shortB = poly([[hexL - 4, y1], [hexL + 2, y1]], false);
  const rightTail = poly([[hexR + 2, y1], [x1 - k, y1], [x1, y1 - k]], false);
  return [top, leftAndStep, shortA, shortB, rightTail];
}

function ticks(w, h, c, pad, side) {
  // 活动页签两侧向外伸出的三道洋红短横
  const midY = h / 2;
  const x = side === "left" ? 0.5 : w - 0.5;
  const dir = side === "left" ? -1 : 1;
  const ys = [midY - 8, midY, midY + 8];
  const lens = [7, 11, 8];
  return ys.map((y, i) => `M${f(x + dir * 1.5)} ${f(y)} H${f(x + dir * (1.5 + lens[i]))}`);
}

function hashMarks(x, y, len, n, down = 3) {
  const ds = [];
  const step = len / (n + 1);
  for (let i = 1; i <= n; i++) {
    const xi = x + step * i;
    ds.push(`M${f(xi)} ${f(y)} V${f(y + down)}`);
  }
  return ds;
}

function draw(svg, w, h, opts) {
  const tone = TONES[opts.tone] || TONES.cyan;
  const kind = opts.kind || "card";
  const isHex = kind === "hex";
  const chamfer = opts.chamfer ?? (isHex ? Math.min(w, h) * 0.46 : kind === "stage" ? 16 : 13);
  const arm = opts.arm ?? (isHex ? Math.min(w, h) * 0.22 : 26);
  const gap = opts.gap ?? 3.2;
  svg.replaceChildren();
  svg.setAttribute("viewBox", `0 0 ${f(w)} ${f(h)}`);
  svg.setAttribute("preserveAspectRatio", "none");
  const g = el("g", { fill: "none", "stroke-linecap": "butt", "stroke-linejoin": "miter" });

  if (kind === "screen") {
    drawScreen(g, w, h, tone);
    svg.append(g);
    return;
  }

  const outer = chamferPts(w, h, chamfer, 1.2);
  const innerPad = 1.2 + gap;
  const innerC = chamfer;
  if (kind === "card" && opts.notch !== false) {
    g.append(el("path", { class: "fr-dim", d: poly(outer), stroke: tone.dim, "stroke-width": 1 }));
    for (const d of brackets(w, h, chamfer, 1.2 + gap * 0.45, arm)) {
      g.append(el("path", { class: "fr-br", d, stroke: tone.bright, "stroke-width": 1.7, filter: "url(#fr-glow)" }));
    }
    for (const d of cardInner(w, h, innerC, innerPad + 0.4)) {
      g.append(el("path", { class: "fr-dim", d, stroke: tone.dim, "stroke-width": 1 }));
    }
  } else if (kind === "stage") {
    // 双线切角框 + 转角托架
    g.append(el("path", { class: "fr-dim", d: poly(outer), stroke: tone.dim, "stroke-width": 1.05 }));
    g.append(el("path", { class: "fr-dim", d: poly(chamferPts(w, h, chamfer, 1.2 + gap)), stroke: tone.dim, "stroke-width": 1 }));
    for (const d of brackets(w, h, chamfer, 1.2 + gap * 0.4, arm * 0.9)) {
      g.append(el("path", { class: "fr-br", d, stroke: tone.bright, "stroke-width": 1.55, filter: "url(#fr-glow)" }));
    }
  } else {
    // hex / dialog / 默认：三线完整周界，托架叠在中缝
    g.append(el("path", { class: "fr-dim", d: poly(outer), stroke: tone.dim, "stroke-width": isHex && opts.ticks ? 1.35 : 1 }));
    for (const d of brackets(w, h, chamfer, 1.2 + gap * 0.45, arm)) {
      g.append(el("path", { class: "fr-br", d, stroke: tone.bright, "stroke-width": isHex && opts.ticks ? 2.05 : (isHex ? 1.25 : 1.65), filter: isHex && !opts.ticks ? null : "url(#fr-glow)" }));
    }
    g.append(el("path", { class: "fr-dim", d: poly(chamferPts(w, h, innerC, innerPad)), stroke: tone.dim, "stroke-width": 1 }));
    if (opts.ticks) {
      for (const side of ["left", "right"]) {
        for (const d of ticks(w, h, chamfer, 1.2, side)) {
          g.append(el("path", { class: "fr-tick", d, stroke: "#ff2bd6", "stroke-width": 1.4 }));
        }
      }
    }
  }

  // 局部 glow（很小的 drop-shadow，不用 blur 滤镜铺满）
  const defs = el("defs", {}, [
    el("filter", { id: "fr-glow", x: "-40%", y: "-40%", width: "180%", height: "180%" }, [
      el("feDropShadow", { dx: "0", dy: "0", "stdDeviation": opts.ticks ? "1.1" : "0.7", "flood-color": tone.bright, "flood-opacity": "0.7" }),
    ]),
  ]);
  svg.append(defs, g);
}

function drawScreen(g, w, h, tone) {
  const p = 6, c = 16, arm = 42;
  const dim = tone.dim, br = tone.bright;
  const topEnd = Math.min(w * 0.48, 220);
  const y = p, x = p;
  g.append(el("path", { d: `M${f(x)} ${f(y + c + arm)} V${f(y + c)} L${f(x + c)} ${f(y)} H${f(topEnd)}`, stroke: dim, "stroke-width": 1.15 }));
  g.append(el("path", { d: `M${f(x + 4)} ${f(y + c + arm * 0.72)} V${f(y + c)} L${f(x + c)} ${f(y + 4)} H${f(topEnd - 12)}`, stroke: br, "stroke-width": 1.6 }));
  const tab = topEnd * 0.58;
  g.append(el("path", { d: `M${f(tab)} ${f(y)} L${f(tab + 6)} ${f(y + 5)} H${f(tab + 20)} L${f(tab + 26)} ${f(y)}`, stroke: dim, "stroke-width": 1.1 }));
  for (const d of hashMarks(x + c + 10, y, 40, 5, 4)) {
    g.append(el("path", { d, stroke: br, "stroke-width": 0.9 }));
  }
  // 顶右，镜像
  const tr = w - p, topStart = Math.max(w * 0.54, w - 200);
  g.append(el("path", { d: `M${f(tr)} ${f(y + c + arm)} V${f(y + c)} L${f(tr - c)} ${f(y)} H${f(topStart)}`, stroke: dim, "stroke-width": 1 }));
  g.append(el("path", { d: `M${f(tr - 3.4)} ${f(y + c + arm * 0.7)} V${f(y + c)} L${f(tr - c)} ${f(y + 3.4)} H${f(topStart + 10)}`, stroke: br, "stroke-width": 1.5 }));
  for (const d of hashMarks(tr - c - 44, y, 36, 4, 3)) {
    g.append(el("path", { d, stroke: dim, "stroke-width": 0.8 }));
  }
  // 底左 / 底右托架 + 中段折线（给文字留空）
  const by = h - p, mag = TONES.magenta;
  g.append(el("path", { d: `M${f(x)} ${f(by - c - arm)} V${f(by - c)} L${f(x + c)} ${f(by)} H${f(w * 0.28)}`, stroke: dim, "stroke-width": 1 }));
  g.append(el("path", { d: `M${f(x + 3.4)} ${f(by - c - arm * 0.65)} V${f(by - c)} L${f(x + c)} ${f(by - 3.4)} H${f(w * 0.22)}`, stroke: br, "stroke-width": 1.45 }));
  g.append(el("path", { d: `M${f(tr)} ${f(by - c - arm)} V${f(by - c)} L${f(tr - c)} ${f(by)} H${f(w * 0.72)}`, stroke: dim, "stroke-width": 1 }));
  g.append(el("path", { d: `M${f(tr - 3.4)} ${f(by - c - arm * 0.65)} V${f(by - c)} L${f(tr - c)} ${f(by - 3.4)} H${f(w * 0.78)}`, stroke: mag.bright, "stroke-width": 1.45 }));
  // 底中装饰折线（断开，不穿过文字）
  const midY = by - 7;
  g.append(el("path", { d: `M${f(w * 0.30)} ${f(midY)} H${f(w * 0.40)} L${f(w * 0.43)} ${f(midY - 5)} H${f(w * 0.57)} L${f(w * 0.60)} ${f(midY)} H${f(w * 0.70)}`, stroke: dim, "stroke-width": 1 }));
}

const attached = new WeakMap();

export function attachFrame(node, opts = {}) {
  if (!node) return () => {};
  let rec = attached.get(node);
  if (rec) {
    rec.opts = { ...rec.opts, ...opts };
    rec.draw();
    return rec.stop;
  }
  const svg = el("svg", { class: "xr-frame", "aria-hidden": "true", style: "position:absolute;inset:0;width:100%!important;height:100%!important;max-width:none;max-height:none;pointer-events:none;z-index:8;overflow:visible;display:block" });
  node.classList.add("has-frame");
  const pos0 = getComputedStyle(node).position;
  if (pos0 === "static") node.style.position = "relative";
  else if (pos0 === "absolute" || pos0 === "fixed") node.style.position = pos0;
  const mount = node.shadowRoot || node;
  if (node.shadowRoot && !node.shadowRoot.getElementById("xr-frame-css")) {
    const st = document.createElement("style");
    st.id = "xr-frame-css";
    st.textContent = ":host{overflow:visible!important;position:relative;} .xr-frame{position:absolute;inset:0;width:100%!important;height:100%!important;max-width:none;max-height:none;pointer-events:none;z-index:8;overflow:visible;display:block}";
    node.shadowRoot.append(st);
  }
  mount.prepend(svg);
  const state = { opts: { ...opts }, svg, ro: null, stop: null, draw: null };
  const redraw = () => {
    const r = node.getBoundingClientRect();
    const w = Math.max(2, r.width);
    const h = Math.max(2, r.height);
    svg.setAttribute("width", String(w));
    svg.setAttribute("height", String(h));
    draw(svg, w, h, state.opts);
  };
  state.draw = redraw;
  const ro = new ResizeObserver(() => redraw());
  state.ro = ro;
  ro.observe(node);
  redraw();
  state.stop = () => { ro.disconnect(); svg.remove(); attached.delete(node); };
  attached.set(node, state);
  return state.stop;
}

export function updateFrame(node, opts) {
  const rec = attached.get(node);
  if (!rec) return attachFrame(node, opts);
  rec.opts = { ...rec.opts, ...opts };
  rec.draw();
}
