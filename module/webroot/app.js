// XRouter WebUI：只通过 ksu.exec 调用 xr。
"use strict";

const XR = "sh /data/adb/modules/xrouter/scripts/xr";
const SET_KEYS = [
  "XR_EXPOSE", "XR_MODE", "VM_SHARE", "VM_SHARE_MODE", "VM_SHARE_PATH",
  "AUTOSTART", "VM_CPUS", "VM_MEM_MB",
];
const ARG_RE = /^[A-Za-z0-9_\-]+$/;
const EXPOSE_OPTS = [
  { value: "off", label: "仅本机" },
  { value: "hotspot", label: "热点设备" },
  { value: "all", label: "当前局域网" },
];
const SHARE_OPTS = [
  { value: "ro", label: "只读" },
  { value: "rw", label: "可读写" },
];
const LOG_NAMES = { module: "模块", console: "控制台", ctl: "控制通道", vmm: "虚拟机管理器" };
const VPN_TXT = { down: "未连接", connecting: "连接中", up: "已连接" };
const UP_KIND = (name) => {
  if (!name) return "无";
  if (/^(rmnet|ccmni|seth|v4-rmnet|v4-ccmni)/i.test(name)) return "蜂窝";
  if (/^wlan/i.test(name)) return "Wi-Fi";
  return name;
};

let status = null;
let seq = 0;
let timer = null;
let logKind = "module";
let page = "home";

const $ = (id) => document.getElementById(id);
const hasKsu = () => typeof window.ksu !== "undefined" && typeof window.ksu.exec === "function";

function shq(v) {
  return "'" + String(v).replace(/'/g, "'\\''") + "'";
}

function exec(cmd) {
  return new Promise((resolve) => {
    if (!hasKsu()) {
      resolve({ errno: 1, stdout: "", stderr: "需在管理器中打开" });
      return;
    }
    const cb = `__xr_cb_${Date.now()}_${++seq}`;
    window[cb] = (errno, stdout, stderr) => {
      delete window[cb];
      resolve({ errno: typeof errno === "number" ? errno : 0, stdout: stdout || "", stderr: stderr || "" });
    };
    try { window.ksu.exec(cmd, "{}", cb); }
    catch (e) {
      delete window[cb];
      resolve({ errno: 1, stdout: "", stderr: String(e) });
    }
  });
}

function xr(sub, ...args) {
  if (!ARG_RE.test(sub)) return Promise.reject(new Error("bad command"));
  return exec(`${XR} ${sub} ${args.map(shq).join(" ")}`.trim());
}

function toast(msg, bad) {
  const t = $("toast");
  t.textContent = msg;
  t.className = "toast " + (bad ? "toast-bad" : "toast-ok");
  clearTimeout(toast.h);
  toast.h = setTimeout(() => t.classList.add("hidden"), 4000);
}

function confirmDlg(message) {
  return new Promise((resolve) => {
    $("modal-text").textContent = message;
    $("modal").classList.remove("hidden");
    const done = (ok) => {
      $("modal").classList.add("hidden");
      $("modal-ok").onclick = null;
      $("modal-cancel").onclick = null;
      resolve(ok);
    };
    $("modal-ok").onclick = () => done(true);
    $("modal-cancel").onclick = () => done(false);
  });
}

async function setKey(key, value) {
  if (!SET_KEYS.includes(key)) return false;
  const r = await xr("set", key, value);
  let res = null;
  try { res = JSON.parse(r.stdout.trim().split("\n").pop()); } catch (e) { res = null; }
  if (!res || !res.ok) {
    toast((res && res.error) || r.stderr || "未保存", true);
    await refresh();
    return false;
  }
  toast(res.restart ? "已保存，需重新启动" : "已保存");
  await refresh();
  return true;
}

function fmtUptime(s) {
  s = Number(s) || 0;
  const h = Math.floor(s / 3600); const m = Math.floor((s % 3600) / 60);
  return h ? `${h} 小时 ${m} 分` : `${m} 分 ${s % 60} 秒`;
}

function vmLabel(vm) {
  if (!vm.running) return "已停止";
  if (vm.state === "ready" || vm.state === "stable") return "运行中";
  return "启动中";
}

function bindSelect(id, opts, current, onChange, disabledValue) {
  const box = $(id);
  box.innerHTML = "";
  const btn = document.createElement("button");
  btn.type = "button";
  btn.className = "cselect-btn";
  const cur = opts.find((o) => o.value === current) || opts[0];
  btn.textContent = cur ? cur.label : "";
  const menu = document.createElement("div");
  menu.className = "cselect-menu hidden";
  opts.forEach((o) => {
    const it = document.createElement("div");
    it.className = "cselect-opt" + (o.value === current ? " on" : "");
    it.textContent = o.label;
    if (o.value === disabledValue) it.setAttribute("aria-disabled", "true");
    it.onclick = () => {
      if (o.value === disabledValue) return;
      menu.classList.add("hidden");
      if (o.value !== current) onChange(o.value);
    };
    menu.appendChild(it);
  });
  btn.onclick = (e) => {
    e.stopPropagation();
    document.querySelectorAll(".cselect-menu").forEach((m) => { if (m !== menu) m.classList.add("hidden"); });
    menu.classList.toggle("hidden");
  };
  box.appendChild(btn);
  box.appendChild(menu);
}

function showPage(name) {
  page = name;
  ["home", "link", "logs", "settings"].forEach((n) => {
    $("page-" + n).classList.toggle("hidden", n !== name);
  });
  document.querySelectorAll(".nav").forEach((b) => b.classList.toggle("on", b.dataset.page === name));
  if (name === "logs") loadLog();
}

function render(d) {
  const vm = d.vm || {}; const net = d.net || {}; const vpn = d.vpn || {}; const sh = d.share || {}; const cfg = d.config || {};
  const label = vmLabel(vm);
  const pill = $("vm-pill");
  pill.textContent = label;
  pill.className = "pill " + (label === "运行中" ? "pill-ok" : label === "启动中" ? "pill-warn" : "pill-off");
  $("st-vm").textContent = label;
  $("st-uptime").textContent = vm.running ? fmtUptime(vm.uptime_s) : "-";
  $("st-up").textContent = UP_KIND(net.up_if);

  const tripped = vm.tripped !== null && vm.tripped !== undefined;
  $("trip-bar").classList.toggle("hidden", !tripped);
  $("btn-start").disabled = tripped || vm.running;
  $("btn-stop").disabled = !vm.running;
  $("btn-expand").disabled = vm.running;
  $("btn-reset").disabled = vm.running;

  const pre = cfg.XR_NET_PREFIX || "172.29.233";
  $("lnk-luci").href = `http://${pre}.254`;
  $("lnk-luci").textContent = `LuCI  http://${pre}.254`;

  $("st-vpn").textContent = VPN_TXT[vpn.status] || vpn.status || "未连接";
  const routes = vpn.routes_active || [];
  $("vpn-active").textContent = routes.length ? routes.join("\n") : "无";

  document.querySelectorAll("input[data-key]").forEach((el) => {
    if (el === document.activeElement) return;
    const v = cfg[el.dataset.key];
    if (v === undefined) return;
    if (el.type === "checkbox") el.checked = v === "1"; else el.value = v;
  });

  bindSelect("sel-expose", EXPOSE_OPTS, cfg.XR_EXPOSE || "hotspot", (v) => setKey("XR_EXPOSE", v));
  bindSelect("sel-share-mode", SHARE_OPTS, cfg.VM_SHARE_MODE || "ro", (v) => setKey("VM_SHARE_MODE", v), sh.rw_supported ? "" : "rw");

  $("btn-mode-global").classList.toggle("on", cfg.XR_MODE !== "bypass");
  $("btn-mode-bypass").classList.toggle("on", cfg.XR_MODE === "bypass");

  const as = $("cb-autostart");
  as.disabled = tripped && cfg.AUTOSTART !== "1";
}

async function refresh() {
  const r = await xr("status-json");
  try {
    status = JSON.parse(r.stdout);
    render(status);
    $("env-badge").textContent = "已连接";
    $("env-badge").className = "badge badge-ok";
  } catch (e) {
    $("env-badge").textContent = hasKsu() ? "读取失败" : "需在管理器中打开";
    $("env-badge").className = "badge badge-bad";
  }
}

function startPolling() {
  stopPolling();
  timer = setInterval(refresh, 5000);
}
function stopPolling() {
  if (timer) clearInterval(timer);
  timer = null;
}

async function action(btn, sub, args, okMsg) {
  if (btn) btn.disabled = true;
  const r = await xr(sub, ...args);
  if (r.errno === 0) toast(okMsg);
  else toast((r.stdout + r.stderr).trim().split("\n").pop() || "失败", true);
  await refresh();
}

async function loadLog() {
  $("log-title").textContent = LOG_NAMES[logKind] || logKind;
  document.querySelectorAll("#log-tabs .tab").forEach((b) => b.classList.toggle("on", b.dataset.log === logKind));
  const r = await xr("log", logKind, "200");
  $("log-box").textContent = r.stdout || "（空）";
  $("log-box").scrollTop = $("log-box").scrollHeight;
}

function toB64(s) {
  const bytes = new TextEncoder().encode(s);
  let bin = "";
  bytes.forEach((b) => { bin += String.fromCharCode(b); });
  return btoa(bin);
}

async function importOvpn(text) {
  const b64 = toB64(text).replace(/\s+/g, "");
  const r0 = await xr("vpn-stage-begin");
  if (r0.errno !== 0) { toast("导入失败", true); return; }
  const size = 1800;
  for (let i = 0; i < b64.length; i += size) {
    const r = await xr("vpn-stage-append", b64.slice(i, i + size));
    if (r.errno !== 0) { toast("导入失败", true); return; }
  }
  const r = await xr("vpn-stage-commit");
  let res = null;
  try { res = JSON.parse((r.stdout || "").trim().split("\n").pop()); } catch (e) { res = null; }
  if (!res || !res.ok) toast((res && res.error) || "导入失败", true);
  else toast("已启用");
  await refresh();
}

function bind() {
  document.querySelectorAll(".nav").forEach((b) => {
    b.onclick = () => showPage(b.dataset.page);
  });
  $("btn-start").onclick = (e) => action(e.target, "start", [], "已启动");
  $("btn-stop").onclick = async (e) => {
    if (!await confirmDlg("停止虚拟机？")) return;
    action(e.target, "stop", [], "已停止");
  };
  $("btn-ack").onclick = (e) => action(e.target, "ack-trip", [], "已确认");
  $("btn-expand").onclick = async (e) => {
    const mb = $("in-expand").value.trim();
    if (!/^\d+$/.test(mb) || Number(mb) < 512 || Number(mb) > 16384) { toast("请输入 512–16384", true); return; }
    if (!await confirmDlg(`将系统盘扩展到 ${mb} MB？`)) return;
    action(e.target, "expand", [mb], "已扩展");
  };
  $("btn-reset").onclick = async (e) => {
    if (!await confirmDlg("重置系统盘？虚拟机内的数据将全部丢失。")) return;
    action(e.target, "reset-disk", [], "已重置");
  };
  $("btn-mode-global").onclick = async () => {
    if ((status && status.config && status.config.XR_MODE) === "global") return;
    if (!await confirmDlg("切换为全局？手机和热点将经 OpenWrt 出网。")) return;
    setKey("XR_MODE", "global");
  };
  $("btn-mode-bypass").onclick = async () => {
    if ((status && status.config && status.config.XR_MODE) === "bypass") return;
    if (!await confirmDlg("切换为旁路？手机和热点将自行出网。")) return;
    setKey("XR_MODE", "bypass");
  };
  $("btn-ovpn").onclick = () => $("in-ovpn").click();
  $("in-ovpn").onchange = () => {
    const f = $("in-ovpn").files && $("in-ovpn").files[0];
    $("in-ovpn").value = "";
    if (!f) return;
    const reader = new FileReader();
    reader.onload = () => importOvpn(String(reader.result || ""));
    reader.readAsText(f);
  };
  document.querySelectorAll("#log-tabs .tab").forEach((b) => {
    b.onclick = () => { logKind = b.dataset.log; loadLog(); };
  });
  $("btn-export-one").onclick = async () => {
    const r = await xr("log-export", logKind);
    let res = null;
    try { res = JSON.parse((r.stdout || "").trim().split("\n").pop()); } catch (e) { res = null; }
    toast(res && res.ok ? "已保存到 Download" : "导出失败", !(res && res.ok));
  };
  $("btn-export-all").onclick = async () => {
    const r = await xr("log-export", "all");
    let res = null;
    try { res = JSON.parse((r.stdout || "").trim().split("\n").pop()); } catch (e) { res = null; }
    toast(res && res.ok ? "已保存到 Download" : "导出失败", !(res && res.ok));
  };
  document.querySelectorAll("input[data-key]").forEach((el) => {
    el.addEventListener("change", () => {
      const v = el.type === "checkbox" ? (el.checked ? "1" : "0") : el.value.trim();
      setKey(el.dataset.key, v);
    });
  });
  document.addEventListener("click", () => {
    document.querySelectorAll(".cselect-menu").forEach((m) => m.classList.add("hidden"));
  });
  document.addEventListener("visibilitychange", () => {
    if (document.hidden) stopPolling(); else { refresh(); startPolling(); }
  });
}

bind();
refresh();
startPolling();
