// XRouter WebUI：只通过 kernelsu exec 调用 xr 命令行
import "@material/web/tabs/tabs.js";
import "@material/web/tabs/primary-tab.js";
import { Tab, ANIMATE_INDICATOR } from "@material/web/tabs/internal/tab.js";
import "@material/web/switch/switch.js";
import "@material/web/button/filled-button.js";
import "@material/web/button/filled-tonal-button.js";
import "@material/web/button/outlined-button.js";
import "@material/web/button/text-button.js";
import "@material/web/iconbutton/icon-button.js";
import "@material/web/dialog/dialog.js";
import "@material/web/icon/icon.js";
import "@material/web/ripple/ripple.js";
import "@material/web/focus/md-focus-ring.js";
import "@material/web/select/outlined-select.js";
import "@material/web/select/select-option.js";
import { exec, isKsuWebui } from "kernelsu-alt";
import "./style.css";
import { ICONS } from "./icons.js";
import {
  SET_KEYS, MODE_OPTS, NOTIFY_OPTS, DEBUG_HOURS_OPTS, ACCESS_OPTS, SHARE_OPTS, CPU_OPTS, MEM_OPTS, LOG_KINDS, VPN_TXT,
  xrCmd, lastJson, upKind, fmtUptime, vmView, displayPath, fmtSize, expandSizes,
  parseBootTail, newBootBuf, feedBoot, trafficLine,
} from "./logic.js";
import { openBrowser } from "./file-browser.js";

const $ = (id) => document.getElementById(id);
let status = null;
let pending = null;      // start | stop
let pendingAt = 0;       // 发出操作的时间，用于兜底超时
let bgArmed = false;     // start-bg 已返回，之后读到的 bg.rc 才属于这次操作
let boot = null;         // 启动日志跟随状态
let ipv6Busy = false;
let logKind = "module";
let page = 0;
let timer = null;

export function icon(el) {
  el.textContent = ICONS[el.dataset.icon] || "";
}
export function setIcon(el, name) {
  el.dataset.icon = name; icon(el);
}

export async function xr(sub, ...args) {
  if (!isKsuWebui()) return { errno: 1, stdout: "", stderr: "需在管理器中打开" };
  try { return await exec(xrCmd(sub, ...args)); }
  catch (e) { return { errno: 1, stdout: "", stderr: String(e) }; }
}

let snackTimer = null;
export function snack(msg, bad) {
  const s = $("snack");
  s.textContent = msg;
  s.className = "snack show" + (bad ? " bad" : "");
  clearTimeout(snackTimer);
  snackTimer = setTimeout(() => { s.className = "snack" + (bad ? " bad" : ""); }, 3000);
}

function dialogResult(dlg) {
  return new Promise((resolve) => {
    const done = () => { dlg.removeEventListener("closed", done); resolve(dlg.returnValue === "ok"); };
    dlg.addEventListener("closed", done);
    dlg.returnValue = "";
    dlg.show();
  });
}

function confirmDlg(title, text, okLabel, danger) {
  $("dlg-confirm-title").textContent = title;
  $("dlg-confirm-text").textContent = text || "";
  $("dlg-confirm-text").hidden = !text;
  const ok = $("dlg-confirm-ok");
  ok.textContent = okLabel || "确认";
  ok.classList.toggle("danger", !!danger);
  return dialogResult($("dlg-confirm"));
}

// ---- 自定义分段选择 ----
function seg(box, opts, current, onPick, disabled) {
  if (box.dataset.sig === JSON.stringify([current, disabled || []])) return;
  box.dataset.sig = JSON.stringify([current, disabled || []]);
  box.replaceChildren(...opts.map((o) => {
    const b = document.createElement("button");
    b.type = "button";
    b.className = "seg-btn" + (o.value === current ? " on" : "");
    b.disabled = (disabled || []).includes(o.value);
    b.innerHTML = "<md-ripple></md-ripple>";
    b.append(o.label);
    b.onclick = () => { if (o.value !== current) onPick(o.value); };
    return b;
  }));
}

function fillSelect(sel, opts, value) {
  if (!sel.dataset.filled) {
    sel.replaceChildren(...opts.map((o) => {
      const it = document.createElement("md-select-option");
      it.value = o.value;
      const h = document.createElement("div"); h.slot = "headline"; h.textContent = o.label;
      it.append(h);
      return it;
    }));
    sel.dataset.filled = "1";
  }
  if (sel.value === value || sel.matches(":focus-within")) return;
  sel.querySelectorAll("md-select-option").forEach((o) => { o.selected = o.value === value; });
  sel.updateComplete.then(() => { if (sel.value !== value) sel.select(value); });
}

async function setKey(key, value) {
  if (!SET_KEYS.includes(key)) return false;
  const r = await xr("set", key, value);
  const res = lastJson(r.stdout);
  if (!res || !res.ok) {
    snack((res && res.error) || r.stderr || "未保存", true);
    await refresh();
    return false;
  }
  if (res.restart) snack("重启虚拟机后生效");
  await refresh();
  return true;
}

// ---- 渲染 ----
function renderPower(d) {
  const v = vmView(d, pending);
  const wrap = $("power-wrap");
  wrap.className = "power-wrap " + v.phase;
  $("power-text").textContent = v.text;
  $("power").disabled = v.phase === "starting" || v.phase === "stopping" || !!(d.vm && d.vm.tripped);
  const steps = $("power-steps");
  steps.hidden = v.phase !== "starting";
  steps.querySelectorAll("[data-step]").forEach((s) => {
    const i = Number(s.dataset.step);
    s.className = i < (v.step || 0) ? "done" : i === v.step ? "cur" : "";
  });
  if (v.phase === "starting") bootOn(); else bootOff();
  const chip = $("chip");
  chip.className = "chip " + (v.chip === "运行中" ? "chip-ok" : v.chip === "异常" ? "chip-bad" : "chip-off");
  $("chip-text").textContent = v.chip;
}

function render(d) {
  status = d;
  const vm = d.vm || {}; const net = d.net || {}; const vpn = d.vpn || {}; const sh = d.share || {}; const cfg = d.config || {};
  if (pending === "start" && (vm.running || vm.error)) pending = vm.running ? null : pending;
  if (pending === "stop" && !vm.running) pending = null;
  // 兜底：后台操作迟迟没有体现在状态里（比如守护进程没回应），不让按钮一直转圈
  if (pending && Date.now() - pendingAt > (pending === "start" ? 120000 : 45000)) {
    snack(pending === "start" ? "启动超时" : "停止超时", true);
    pending = null;
  }
  renderPower(d);
  const tripped = vm.tripped !== null && vm.tripped !== undefined;
  $("trip").hidden = !tripped;

  const as = $("sw-autostart");
  as.selected = cfg.AUTOSTART === "1";
  as.disabled = tripped && cfg.AUTOSTART !== "1";
  $("row-autostart").classList.toggle("disabled", as.disabled);

  $("v-uptime").textContent = vm.running ? fmtUptime(vm.uptime_s) : "—";
  const kind = vm.running ? upKind(net.up_if) : "—";
  $("v-up").textContent = kind;
  setIcon($("i-up"), kind === "Wi-Fi" ? "wifi" : kind === "蜂窝" ? "signal_cellular_alt" : "cloud_off");

  $("v-vpn").textContent = VPN_TXT[vpn.status] || "未连接";
  const routes = vpn.routes_active || [];
  $("v-routes").textContent = routes.length ? routes.join("\n") : "无";

  seg($("seg-mode"), MODE_OPTS, cfg.XR_MODE || "global", pickMode);
  fillSelect($("sel-access"), ACCESS_OPTS, cfg.XR_EXPOSE || "lan");
  const v6 = $("sw-ipv6");
  if (!ipv6Busy) v6.selected = d.ipv6 === true;
  v6.disabled = ipv6Busy || d.ipv6 === null || d.ipv6 === undefined;
  $("row-ipv6").classList.toggle("disabled", v6.disabled);

  $("sw-share").selected = cfg.VM_SHARE === "1";
  seg($("seg-share"), SHARE_OPTS, cfg.VM_SHARE_MODE || "ro", (v) => setKey("VM_SHARE_MODE", v), sh.rw_supported ? [] : ["rw"]);
  $("v-share-path").textContent = displayPath(cfg.VM_SHARE_PATH);

  seg($("seg-notify"), NOTIFY_OPTS, cfg.XR_NOTIFY_TRAFFIC || "total", (v) => setKey("XR_NOTIFY_TRAFFIC", v));
  $("sw-debug").selected = cfg.XR_DEBUG === "1";
  $("sw-hotspot").selected = cfg.XR_HOTSPOT_BRIDGE !== "0";
  $("v-hotspot").textContent = net.hotspot ? `已接管 ${net.hotspot}` : "";
  fillSelect($("sel-debug-hours"), DEBUG_HOURS_OPTS, cfg.XR_DEBUG_HOURS || "24");

  fillSelect($("sel-cpu"), CPU_OPTS, cfg.VM_CPUS || "2");
  fillSelect($("sel-mem"), MEM_OPTS, cfg.VM_MEM_MB || "512");
  $("v-disk").textContent = fmtSize(cfg.VM_DISK_SIZE_MB);
  $("btn-expand").disabled = !!vm.running;
  $("btn-reset").disabled = !!vm.running;
}

async function refresh() {
  const r = await xr("status-json");
  let d = null;
  try { d = JSON.parse(r.stdout); } catch { d = null; }
  if (!d) {
    $("chip").className = "chip chip-bad";
    $("chip-text").textContent = "异常";
    return;
  }
  render(d);
}

function schedule() {
  clearTimeout(timer);
  const v = status ? vmView(status, pending) : { phase: "stopped" };
  const fast = v.phase === "starting" || v.phase === "stopping";
  timer = setTimeout(async () => {
    if (!document.hidden) { await refresh(); if (page === 2) loadLog(true); if (page === 3) loadTraffic(); }
    schedule();
  }, fast ? 1500 : 5000);
}

// ---- 启动日志：增量读取（按字节偏移），约 400ms 一次，最多 20 行，单个文本节点整体替换 ----
function bootOn() {
  if (boot) return;
  // 点击启动：串口日志会被 start-bg 清空，从当前末尾起跟随，清空后自动从 0 读；
  // 打开页面时已在开机：直接给出串口日志末尾一段
  boot = { m: -1, c: pending === "start" ? -1 : 0, buf: newBootBuf(), timer: 0 };
  const el = $("power-log");
  el.textContent = "";
  el.hidden = false;
  bootTick(boot);
}

function bootOff() {
  if (!boot) return;
  clearTimeout(boot.timer);
  boot = null;
  const el = $("power-log");
  el.hidden = true;
  el.textContent = "";
}

async function bootTick(b) {
  if (boot !== b) return;
  if (!document.hidden && page === 0) {
    const r = await xr("boot-tail", String(b.m), String(b.c));
    if (boot !== b) return;
    const t = parseBootTail(r.stdout);
    if (t) {
      b.m = t.mod.size; b.c = t.con.size;
      const a = feedBoot(b.buf, "mod", t.mod.text);
      const c = feedBoot(b.buf, "con", t.con.text);
      if (a || c) $("power-log").textContent = b.buf.lines.join("\n");
      if (pending === "start" && bgArmed && t.rc !== null && t.rc !== 0) {
        // 后台 xr start 已失败退出：立刻结束转圈，显示原因
        pending = null;
        await refresh();
        if (!(status && status.vm && (status.vm.running || status.vm.error))) snack("启动失败", true);
        schedule();
        return;
      }
    }
  }
  if (boot === b) b.timer = setTimeout(() => bootTick(b), 400);
}

// ---- 操作 ----
// 点击只切换界面状态并投递后台命令，xr start/stop 在脱离会话的子进程里跑，按钮从不等它
function act(kind) {
  pending = kind; pendingAt = Date.now(); bgArmed = false;
  renderPower(status || {});
  schedule();
  (kind === "start" ? xr("start-bg") : xr("stop-bg")).then((r) => {
    const res = lastJson(r.stdout);
    if (r.errno !== 0 || !res || !res.ok) {
      pending = null;
      snack(kind === "start" ? "启动失败" : "停止失败", true);
      refresh().then(schedule);
      return;
    }
    bgArmed = true;
  });
}

async function power() {
  const v = vmView(status || {}, pending);
  if (v.phase === "starting" || v.phase === "stopping") return;
  if (v.phase === "running" || (status && status.vm && status.vm.running)) {
    if (!await confirmDlg("停止虚拟机", "", "停止", true)) return;
    act("stop");
  } else {
    act("start");
  }
}

async function pickMode(v) {
  const label = MODE_OPTS.find((o) => o.value === v).label;
  if (!await confirmDlg(`切换为${label}`, "", "切换")) return;
  setKey("XR_MODE", v);
}

async function toggleIpv6() {
  const sw = $("sw-ipv6");
  const want = sw.selected ? "on" : "off";
  ipv6Busy = true; sw.disabled = true; $("row-ipv6").classList.add("busy");
  const r = await xr("ipv6", want);
  const res = lastJson(r.stdout);
  ipv6Busy = false; $("row-ipv6").classList.remove("busy");
  if (!res || !res.ok) snack((res && res.error) || "切换失败", true);
  await refresh();
}

async function importVpn() {
  const path = await openBrowser({ mode: "ovpn", xr });
  if (!path) return;
  const r = await xr("vpn-import", path);
  const res = lastJson(r.stdout);
  if (res && res.ok) snack("已导入"); else snack((res && res.error) || "导入失败", true);
  await refresh();
}

async function pickSharePath() {
  const cur = status && status.config ? status.config.VM_SHARE_PATH : "";
  const path = await openBrowser({ mode: "dir", start: cur, xr });
  if (path) setKey("VM_SHARE_PATH", path);
}

async function expandDisk() {
  const cur = Number(status && status.config && status.config.VM_DISK_SIZE_MB) || 1024;
  const sizes = expandSizes(cur);
  if (!sizes.length) { snack("已达上限", true); return; }
  const sel = $("sel-expand");
  delete sel.dataset.filled;
  fillSelect(sel, sizes.map((n) => ({ value: String(n), label: fmtSize(n) })), String(sizes[0]));
  sel.value = String(sizes[0]);
  if (!await dialogResult($("dlg-expand"))) return;
  const r = await xr("expand", sel.value);
  snack(r.errno === 0 ? "已扩容" : "扩容失败", r.errno !== 0);
  await refresh();
}

async function resetDisk() {
  if (!await confirmDlg("重置系统盘", "OpenWrt 内的数据将全部清除", "重置", true)) return;
  const r = await xr("reset-disk");
  snack(r.errno === 0 ? "已重置" : "重置失败", r.errno !== 0);
  await refresh();
}

// ---- 累计流量：只在设置页可见时读取 ----
async function loadTraffic() {
  const r = await xr("traffic");
  $("v-traffic").textContent = trafficLine(lastJson(r.stdout), "total");
}
async function resetTraffic() {
  if (!await confirmDlg("清零流量统计", "通知与此处的累计流量将从 0 重新计算", "清零", true)) return;
  const r = await xr("traffic-reset");
  const res = lastJson(r.stdout);
  snack(res && res.ok ? "已清零" : "清零失败", !(res && res.ok));
  loadTraffic();
}

// ---- 日志 ----
async function loadLog(keepScroll) {
  const box = $("log-box");
  const atEnd = box.scrollTop + box.clientHeight >= box.scrollHeight - 8;
  const r = await xr("log", logKind, "300");
  box.textContent = r.stdout || "";
  box.classList.toggle("empty", !r.stdout);
  if (!keepScroll || atEnd) box.scrollTop = box.scrollHeight;
}
async function exportLog(kind) {
  const r = await xr("log-export", kind);
  const res = lastJson(r.stdout);
  snack(res && res.ok ? "已保存到 Download" : "导出失败", !(res && res.ok));
}
let diagBusy = false;
async function exportDiag() {
  if (diagBusy) return;
  diagBusy = true; $("btn-diag").disabled = true;
  snack("正在收集诊断信息…");
  const r = await xr("diag-export");
  const res = lastJson(r.stdout);
  diagBusy = false; $("btn-diag").disabled = false;
  snack(res && res.ok ? "诊断包已保存到 Download" : "导出失败", !(res && res.ok));
}
async function clearLog() {
  const label = LOG_KINDS.find((k) => k.value === logKind).label;
  if (!await confirmDlg(`清空${label}日志`, "", "清空", true)) return;
  await xr("log-clear", logKind);
  loadLog();
}
function renderLogKinds() {
  seg($("log-kinds"), LOG_KINDS, logKind, (v) => { logKind = v; renderLogKinds(); loadLog(); });
}

// ---- 页面切换：标题栏固定，只切换内容；点击 Tab 或左右滑动，均瞬时切换、不做任何过渡 ----
// 关掉 Material Tab 指示条的 250ms 滑动动画：激活态由 CSS :host([active]) 直接显示
Tab.prototype[ANIMATE_INDICATOR] = function () {};

function goPage(i) {
  i = Math.max(0, Math.min(3, i));
  const pages = document.querySelectorAll("#pager > .page");
  pages.forEach((p, k) => p.classList.toggle("on", k === i));
  const tabs = $("tabs");
  if (tabs.activeTabIndex !== i) tabs.activeTabIndex = i;
  if (page === i) return;
  page = i;
  if (i === 2) loadLog();
  if (i === 3) loadTraffic();
}

function bindPager() {
  const pager = $("pager");
  const tabs = $("tabs");
  tabs.addEventListener("change", () => goPage(tabs.activeTabIndex));
  // 只识别明确的横向轻扫：不跟手、不吸附，松手即跳页
  let sx = 0; let sy = 0; let st = 0; let track = false;
  pager.addEventListener("touchstart", (e) => {
    if (e.touches.length !== 1) { track = false; return; }
    const t = e.touches[0]; sx = t.clientX; sy = t.clientY; st = e.timeStamp; track = true;
  }, { passive: true });
  pager.addEventListener("touchend", (e) => {
    if (!track) return;
    track = false;
    const t = e.changedTouches[0];
    const dx = t.clientX - sx; const dy = t.clientY - sy;
    if (Math.abs(dx) < 56 || Math.abs(dx) < Math.abs(dy) * 1.6 || e.timeStamp - st > 700) return;
    goPage(page + (dx < 0 ? 1 : -1));
  }, { passive: true });
  pager.addEventListener("touchcancel", () => { track = false; }, { passive: true });
}

function bindRowSwitch(row, sw, onChange) {
  row.addEventListener("click", (e) => {
    if (sw.disabled) return;
    if (e.target !== sw) { e.preventDefault(); sw.selected = !sw.selected; }
    onChange();
  });
  sw.addEventListener("click", (e) => e.stopPropagation());
  sw.addEventListener("change", onChange);
}

function bind() {
  document.querySelectorAll("md-icon[data-icon]").forEach(icon);
  bindPager();
  goPage(0);
  $("power").addEventListener("click", power);
  $("btn-ack").addEventListener("click", async () => { await xr("ack-trip"); await refresh(); });
  bindRowSwitch($("row-autostart"), $("sw-autostart"), () => setKey("AUTOSTART", $("sw-autostart").selected ? "1" : "0"));
  bindRowSwitch($("row-share"), $("sw-share"), () => setKey("VM_SHARE", $("sw-share").selected ? "1" : "0"));
  bindRowSwitch($("row-ipv6"), $("sw-ipv6"), toggleIpv6);
  bindRowSwitch($("row-hotspot"), $("sw-hotspot"), () => setKey("XR_HOTSPOT_BRIDGE", $("sw-hotspot").selected ? "1" : "0"));
  bindRowSwitch($("row-debug"), $("sw-debug"), () => setKey("XR_DEBUG", $("sw-debug").selected ? "1" : "0"));
  $("sel-debug-hours").addEventListener("change", (e) => setKey("XR_DEBUG_HOURS", e.target.value));
  $("btn-diag").addEventListener("click", exportDiag);
  $("btn-luci").addEventListener("click", () => xr("luci"));
  $("btn-import").addEventListener("click", importVpn);
  $("btn-share-path").addEventListener("click", pickSharePath);
  $("sel-access").addEventListener("change", (e) => setKey("XR_EXPOSE", e.target.value));
  $("sel-cpu").addEventListener("change", (e) => setKey("VM_CPUS", e.target.value));
  $("sel-mem").addEventListener("change", (e) => setKey("VM_MEM_MB", e.target.value));
  $("btn-expand").addEventListener("click", expandDisk);
  $("btn-reset").addEventListener("click", resetDisk);
  $("btn-traffic-reset").addEventListener("click", resetTraffic);
  $("btn-export-one").addEventListener("click", () => exportLog(logKind));
  $("btn-export-all").addEventListener("click", () => exportLog("all"));
  $("btn-log-clear").addEventListener("click", clearLog);
  renderLogKinds();
  document.addEventListener("visibilitychange", () => { if (!document.hidden) { refresh(); schedule(); } });
}

// ---- 全屏：SukiSU WebUI 默认给页面四周留出系统栏（灰色 #303030 条）。
// 请求管理器内置的 /internal/insets.css 即开启边到边（SuFilePathHandler 据此置 isInsetsEnabled），
// 该样式表给出 --window-inset-top / --window-inset-bottom 等安全区变量；再调用 ksu.enableEdgeToEdge(true) 兜底。
// 普通浏览器里两者都不存在，CSS 回退到 env(safe-area-inset-*)。
function edgeToEdge() {
  if (!isKsuWebui()) return;
  const l = document.createElement("link");
  l.rel = "stylesheet"; l.href = "/internal/insets.css";
  document.head.append(l);
  try { if (window.ksu && typeof window.ksu.enableEdgeToEdge === "function") window.ksu.enableEdgeToEdge(true); } catch { /* 旧管理器 */ }
}

(async () => {
  edgeToEdge();
  bind();
  await customElements.whenDefined("md-tabs");
  document.querySelector(".shell").removeAttribute("unresolved");
  await refresh();
  schedule();
})();
