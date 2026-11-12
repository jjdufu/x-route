// XRouter WebUI：纯函数（不依赖 DOM / ksu，可在 node 中测试）
export const XR = "sh /data/adb/modules/xrouter/scripts/xr";
export const SET_KEYS = [
  "XR_EXPOSE", "XR_MODE", "VM_SHARE", "VM_SHARE_MODE", "VM_SHARE_PATH",
  "AUTOSTART", "VM_CPUS", "VM_MEM_MB", "XR_NOTIFY_TRAFFIC", "XR_DEBUG", "XR_DEBUG_HOURS", "XR_HOTSPOT_BRIDGE", "XR_HOTSPOT_SUBNET",
];
const ARG_RE = /^[A-Za-z0-9_-]+$/;
export const MEDIA_ROOT = "/storage/emulated/0";
export const MEDIA_LABEL = "内部存储";

export const MODE_OPTS = [
  { value: "global", label: "全局接管" },
  { value: "bypass", label: "旁路网关" },
];
export const ACCESS_OPTS = [
  { value: "local", label: "本机" },
  { value: "lan", label: "内网" },
  { value: "all", label: "所有" },
];
export const SHARE_OPTS = [
  { value: "ro", label: "只读" },
  { value: "rw", label: "读写" },
];
export const CPU_OPTS = [1, 2, 3, 4].map((n) => ({ value: String(n), label: `${n} 核` }));
export const MEM_OPTS = [384, 512, 768, 1024, 1536, 2048].map((n) => ({ value: String(n), label: n >= 1024 ? `${n / 1024} GB` : `${n} MB` }));
export const DEBUG_HOURS_OPTS = [
  { value: "1", label: "1 小时" },
  { value: "6", label: "6 小时" },
  { value: "24", label: "24 小时" },
  { value: "0", label: "不关闭" },
];
export const NOTIFY_OPTS = [
  { value: "total", label: "总流量" },
  { value: "wan", label: "公网" },
  { value: "lan", label: "内网" },
];
export const VPN_TXT = { down: "未连接", connecting: "连接中", up: "已连接" };
export const STEPS = ["正在启动", "网络就绪", "运行中"];

export function shq(v) {
  return "'" + String(v).replace(/'/g, "'\\''") + "'";
}

export function xrCmd(sub, ...args) {
  if (!ARG_RE.test(sub)) throw new Error("bad command");
  return `${XR} ${sub} ${args.map(shq).join(" ")}`.trim();
}

export function lastJson(text) {
  const lines = String(text || "").trim().split("\n");
  try { return JSON.parse(lines[lines.length - 1]); } catch { return null; }
}

export function upKind(name) {
  if (!name) return "无";
  if (/^(rmnet|ccmni|seth|v4-rmnet|v4-ccmni)/i.test(name)) return "蜂窝";
  if (/^wlan/i.test(name)) return "Wi-Fi";
  return name;
}

export function fmtUptime(s) {
  s = Number(s) || 0;
  const d = Math.floor(s / 86400); const h = Math.floor((s % 86400) / 3600); const m = Math.floor((s % 3600) / 60);
  if (d) return `${d} 天 ${h} 小时`;
  if (h) return `${h} 小时 ${m} 分`;
  return `${m} 分 ${s % 60} 秒`;
}

export function labelOf(opts, value) {
  const o = opts.find((x) => x.value === value);
  return o ? o.label : "";
}

// 由后端状态推导电源按钮：phase = stopped | starting | stopping | running | failed
// pending：WebUI 已发出、后端尚未体现的操作（start / stop）
export function vmView(d, pending) {
  const vm = (d && d.vm) || {}; const net = (d && d.net) || {}; const cfg = (d && d.config) || {};
  const tripped = vm.tripped !== null && vm.tripped !== undefined;
  if (vm.running) {
    if (pending === "stop") return { phase: "stopping", chip: "运行中", text: "正在停止" };
    if (vm.state !== "ready" && vm.state !== "stable") return { phase: "starting", chip: "运行中", text: STEPS[0], step: 0 };
    if ((cfg.XR_MODE || "global") === "global" && cfg.VM_NET !== "0" && (net.gw || "off") === "off") {
      return { phase: "starting", chip: "运行中", text: STEPS[1], step: 1 };
    }
    return { phase: "running", chip: "运行中", text: STEPS[2], step: 2 };
  }
  if (tripped) return { phase: "failed", chip: "异常", text: "整机复位后自启已关闭" };
  if (pending === "start") return { phase: "starting", chip: "已停止", text: STEPS[0], step: 0 };
  if (vm.error) return { phase: "failed", chip: "异常", text: vm.error };
  return { phase: "stopped", chip: "已停止", text: "已停止" };
}

// 内部存储路径 <-> 显示
export function toMediaPath(p) {
  p = String(p || "");
  for (const root of ["/data/media/0", "/sdcard", MEDIA_ROOT]) {
    if (p === root) return MEDIA_ROOT;
    if (p.startsWith(root + "/")) return MEDIA_ROOT + p.slice(root.length);
  }
  return MEDIA_ROOT;
}
export function displayPath(p) {
  const m = toMediaPath(p);
  return MEDIA_LABEL + m.slice(MEDIA_ROOT.length);
}
export function crumbs(p) {
  const m = toMediaPath(p);
  const parts = m.slice(MEDIA_ROOT.length).split("/").filter(Boolean);
  const out = [{ label: MEDIA_LABEL, path: MEDIA_ROOT }];
  let cur = MEDIA_ROOT;
  for (const s of parts) { cur += "/" + s; out.push({ label: s, path: cur }); }
  return out;
}
export function parseLs(text) {
  return String(text || "").split("\n").filter((l) => /^[df]\//.test(l))
    .map((l) => ({ dir: l[0] === "d", name: l.slice(2) }))
    .filter((e) => e.name && !e.name.startsWith("."));
}

// 扩容可选大小：大于当前、步进到上限 16384
export function expandSizes(cur) {
  cur = Number(cur) || 1024;
  const out = [];
  for (const add of [512, 1024, 2048, 4096, 8192]) {
    const v = cur + add;
    if (v <= 16384) out.push(v);
  }
  return out;
}
export function fmtSize(mb) {
  mb = Number(mb) || 0;
  return mb >= 1024 ? `${+(mb / 1024).toFixed(1)} GB` : `${mb} MB`;
}

// ---- 启动动画：按钮背景里滚动的真实启动日志（xr boot-tail） ----
// 开机日志缓冲：保留最近 400 行做「待滚动」积压（一次轮询可能到几十上百行），界面只显示末尾 16 行
export const BOOT_LINES = 400;
export const BOOT_COLS = 64;
const SEP = "\x1f";

// 输出格式：R<退出码|-> SEP S<虚拟机状态> SEP 模块日志大小 SEP 新增 SEP 串口日志大小 SEP 新增
// （S 段可省略：旧格式直接是模块日志大小）
export function parseBootTail(text) {
  const f = String(text || "").split(SEP);
  if (f.length < 5 || f[0][0] !== "R") return null;
  const rc = f[0].slice(1).trim();
  let st = null;
  if (f[1][0] === "S") { st = f[1].slice(1).trim() || null; f.splice(1, 1); }
  if (f.length < 5) return null;
  return {
    rc: rc === "" || rc === "-" || isNaN(Number(rc)) ? null : Number(rc),
    st,
    mod: { size: Number(f[1]) || 0, text: f[2] },
    con: { size: Number(f[3]) || 0, text: f.slice(4).join("") },
  };
}

export function newBootBuf() {
  return { lines: [], carry: { mod: "", con: "" }, total: 0 };
}

// 并入一段新增文本：各来源保留未完结的半行；去掉颜色码与模块日志的时间前缀；
// 只留最后 cap 行、每行最多 BOOT_COLS 字，DOM 永远很小。返回是否有新行。
export function feedBoot(buf, src, text, cap = BOOT_LINES) {
  if (!text) return false;
  const parts = (buf.carry[src] + text).split("\n");
  buf.carry[src] = parts.pop();
  if (buf.carry[src].length > 512) { parts.push(buf.carry[src]); buf.carry[src] = ""; }
  let changed = false;
  for (let l of parts) {
    l = l.replace(/[\x00-\x08\x0b-\x1f\x7f]/g, "").replace(/\[\d+(?:;\d+)*m/g, "").replace(/\t/g, "  ").trimEnd();
    if (src === "mod") l = l.replace(/^\d{4}-\d\d-\d\d \d\d:\d\d:\d\d \[(?:INFO|ERROR)\] /, "");
    if (!l.trim()) continue;
    buf.lines.push(l.length > BOOT_COLS ? l.slice(0, BOOT_COLS) : l);
    buf.total++;      // 累计行数：界面据此算出这次新增了几行，做平滑上移
    changed = true;
  }
  if (buf.lines.length > cap) buf.lines.splice(0, buf.lines.length - cap);
  return changed;
}

// 累计流量：与通知、traffic.sh 一致（1024 进制、一位小数）
const UNITS = ["B", "KB", "MB", "GB", "TB"];
export function fmtBytes(n) {
  let v = Math.max(0, Number(n) || 0);
  let i = 0;
  while (v >= 1024 && i < UNITS.length - 1) { v /= 1024; i++; }
  return i === 0 ? `${Math.floor(v)} B` : `${v.toFixed(1)} ${UNITS[i]}`;
}
// d = xr traffic 的 JSON；mode = total | wan | lan
export function trafficLine(d, mode) {
  const t = d && d[mode === "wan" || mode === "lan" ? mode : "total"];
  if (!t) return "—";
  return `↑${fmtBytes(t.up)} ↓${fmtBytes(t.down)}`;
}

// 组网页配置卡片列表：后端 vpn.profiles 每项对应一份已导入配置（含独立 id、名称、启停状态、地址与网段），
// 未导入任何配置时返回空列表（界面展示空状态引导），同时兼容客侧 LuCI 手工建立的额外隧道（managed: false）。
export function vpnProfiles(vpn) {
  vpn = vpn || {};
  const active = Array.isArray(vpn.routes_active) ? vpn.routes_active : [];
  const matchRoutes = (own, fallbackAll) => {
    if (own.length) return active.filter((a) => own.some((r) => r === a || `${r}/32` === a || `${r}/128` === a));
    return fallbackAll ? active : [];
  };
  const out = [];
  const usedDevs = new Set();

  if (Array.isArray(vpn.profiles)) {
    const upCount = vpn.profiles.filter((p) => p && p.status === "up").length;
    for (const p of vpn.profiles) {
      if (!p || !p.id) continue;
      const st = VPN_TXT[p.status] ? p.status : "down";
      const own = Array.isArray(p.routes) ? p.routes : [];
      if (p.dev) usedDevs.add(p.dev);
      out.push({
        key: `p:${p.id}`,
        id: p.id,
        name: p.name || p.id,
        enabled: !!p.enabled,
        status: st,
        text: VPN_TXT[st],
        dev: p.dev || "",
        addr: String(p.addr || "").replace(/\/\d+$/, ""),
        routes: st === "up" ? matchRoutes(own, upCount <= 1) : [],
        managed: true,
      });
    }
  } else if (vpn.configured) {
    const t0 = Array.isArray(vpn.tunnels) && vpn.tunnels[0];
    const st = VPN_TXT[vpn.status] ? vpn.status : "down";
    if (t0 && t0.dev) usedDevs.add(t0.dev);
    out.push({
      key: "p:legacy",
      id: "",
      name: (t0 && t0.name) || "OpenVPN",
      enabled: vpn.enabled !== false,
      status: st,
      text: VPN_TXT[st],
      dev: (t0 && t0.dev) || "",
      addr: String((t0 && t0.addr) || vpn.addr || "").replace(/\/\d+$/, ""),
      routes: st === "up" ? active : [],
      managed: true,
    });
  }

  if (Array.isArray(vpn.tunnels)) {
    const totalTuns = vpn.tunnels.length;
    vpn.tunnels.forEach((t, i) => {
      if (!t || !t.dev || usedDevs.has(t.dev)) return;
      const st = VPN_TXT[t.status] ? t.status : "up";
      const own = Array.isArray(t.routes) ? t.routes : [];
      out.push({
        key: `t:${t.dev || i}`,
        id: "",
        name: t.name || t.dev,
        enabled: true,
        status: st,
        text: VPN_TXT[st],
        dev: t.dev,
        addr: String(t.addr || "").replace(/\/\d+$/, ""),
        routes: st === "up" ? matchRoutes(own, totalTuns <= 1 && out.length === 0) : [],
        managed: false,
      });
    });
  }

  return out;
}

// 兼容旧测试与隧道视图：后端 vpn.tunnels 优先，未连接时回退单条汇总
export function vpnTunnels(vpn) {
  vpn = vpn || {};
  const active = vpn.routes_active || [];
  const list = Array.isArray(vpn.tunnels) && vpn.tunnels.length ? vpn.tunnels
    : [{ dev: "", name: "OpenVPN", status: vpn.status || "down", addr: vpn.addr || "", routes: active }];
  const multi = list.length > 1;
  return list.map((t, i) => {
    const st = VPN_TXT[t.status] ? t.status : "down";
    const own = Array.isArray(t.routes) ? t.routes : [];
    const routes = multi ? active.filter((a) => own.some((r) => r === a || `${r}/32` === a || `${r}/128` === a)) : active;
    return { key: t.dev || `#${i}`, name: t.name || "OpenVPN", status: st, text: VPN_TXT[st], addr: String(t.addr || "").replace(/\/\d+$/, ""), routes };
  });
}

// 累计流量弹窗：手机本机 / 热点设备各三项（总流量 | 公网 | 内网），口径与保活通知一致
export function splitLines(d) {
  const one = (x) => ({ total: trafficLine(x, "total"), wan: trafficLine(x, "wan"), lan: trafficLine(x, "lan") });
  if (!d || !d.ok) return null;
  return { phone: one(d.phone), hotspot: one(d.hotspot) };
}

// 固定热点网段（与手机侧 hs_subnet_valid 一致的前端预检；冲突检查以手机侧为准）
export function hsSubnetCheck(v) {
  v = String(v || "").trim();
  if (!v) return { ok: true, value: "" };
  const m = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})\/(\d{1,2})$/.exec(v);
  if (!m) return { ok: false, error: "格式：地址/掩码，如 172.28.222.254/24" };
  const o = m.slice(1, 5).map(Number); const len = Number(m[5]);
  if (o.some((x) => x > 255)) return { ok: false, error: "地址无效" };
  if (len < 16 || len > 30) return { ok: false, error: "掩码需在 16–30 之间" };
  const priv = o[0] === 10 || (o[0] === 172 && o[1] >= 16 && o[1] <= 31) || (o[0] === 192 && o[1] === 168 && len >= 16);
  if (!priv) return { ok: false, error: "需为私网地址（10/8、172.16/12、192.168/16）" };
  const ip = ((o[0] << 24) | (o[1] << 16) | (o[2] << 8) | o[3]) >>> 0;
  const host = ip & ((2 ** (32 - len)) - 1);
  if (host === 0 || host === 2 ** (32 - len) - 1) return { ok: false, error: "网关不能是网络地址或广播地址" };
  return { ok: true, value: v };
}

// 瀑布滚动速度（单位：行 / 帧）：积压越多越快，至少 0.12 行 / 帧（约 7 行 / 秒，保持「在滚」），
// 最多 1.5 行 / 帧（约 90 行 / 秒）；积压超出缓冲（落后太多）直接跳到末尾一屏。
// pos 是已滚到的位置（可为小数），target 是累计行数；返回下一帧的位置
export const BOOT_MIN_STEP = 0.12;
export const BOOT_MAX_STEP = 1.5;
export function bootStep(pos, target, rows = 16) {
  const back = target - pos;
  if (back <= 0) return target;
  if (back > BOOT_LINES - rows) return target - rows;
  return Math.min(target, pos + Math.min(BOOT_MAX_STEP, Math.max(BOOT_MIN_STEP, back * 0.18)));
}
