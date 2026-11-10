// XRouter WebUI：纯函数（不依赖 DOM / ksu，可在 node 中测试）
export const XR = "sh /data/adb/modules/xrouter/scripts/xr";
export const SET_KEYS = [
  "XR_EXPOSE", "XR_MODE", "VM_SHARE", "VM_SHARE_MODE", "VM_SHARE_PATH",
  "AUTOSTART", "VM_CPUS", "VM_MEM_MB",
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
export const LOG_KINDS = [
  { value: "module", label: "模块" },
  { value: "console", label: "串口" },
  { value: "ctl", label: "控制服务" },
  { value: "vmm", label: "虚拟机" },
];
export const VPN_TXT = { down: "未连接", connecting: "连接中", up: "已连接" };
export const STEPS = ["启动虚拟机", "网络就绪", "运行中"];

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
