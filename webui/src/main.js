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
import { exec, spawn, isKsuWebui } from "kernelsu-alt";
import "./style.css";
import "./pcb.css";
import { PCB_FLOW } from "./pcb.js";
import { ICONS } from "./icons.js";
import {
  SET_KEYS, MODE_OPTS, NOTIFY_OPTS, DEBUG_HOURS_OPTS, ACCESS_OPTS, SHARE_OPTS, CPU_OPTS, MEM_OPTS, VPN_TXT,
  xrCmd, lastJson, upKind, fmtUptime, vmView, displayPath, fmtSize, expandSizes,
  parseBootTail, newBootBuf, feedBoot, bootStep, trafficLine, hsSubnetCheck,
} from "./logic.js";
import { openBrowser } from "./file-browser.js";

const $ = (id) => document.getElementById(id);
// 只在值真的变化时写 DOM：5 秒一次的状态刷新大多数时候什么都不改，不触发样式重算与重排
const txt = (el, v) => { v = String(v); if (el.textContent !== v) el.textContent = v; };
const cls = (el, v) => { if (el.className !== v) el.className = v; };
const prop = (el, k, v) => { if (el[k] !== v) el[k] = v; };
const hide = (el, v) => { if (el.hidden !== v) el.hidden = v; };
const tog = (el, c, v) => { if (el.classList.contains(c) !== v) el.classList.toggle(c, v); };
let status = null;
let pending = null;      // start | stop
let pendingAt = 0;       // 发出操作的时间，用于兜底超时
let bgArmed = false;     // start-bg 已返回，之后读到的 bg.rc 才属于这次操作
let boot = null;         // 启动日志跟随状态
let ipv6Busy = false;
let page = 0;
let timer = null;
let gone = false;         // 已请求立即卸载：停止一切轮询

export function icon(el) {
  el.textContent = ICONS[el.dataset.icon] || "";
}
export function setIcon(el, name) {
  el.dataset.icon = name; icon(el);
}

// 卡顿根因：旧版管理器（SukiSU 2026-10 之前，含手机上的 v4.1.3）的 ksu.exec 是同步 JavascriptInterface：
// 新建 root shell + 命令执行完才返回，期间 WebView 主线程（JS、样式、布局、输入）整个被堵住；
// status-json 在手机上要 ~800ms，每 5 秒一次、每次点设置后再来一次，点击 / 切页 / 滑动自然一顿一顿。
// ksu.spawn 只同步创建 shell（约 20ms），命令在后台线程跑、输出异步回调，所以优先用 spawn；没有时退回 exec。
function runSpawn(cmd) {
  return new Promise((resolve) => {
    const out = []; const err = []; let done = false;
    const end = (code) => { if (!done) { done = true; resolve({ errno: code, stdout: out.join("\n"), stderr: err.join("\n") }); } };
    let c;
    try { c = spawn(cmd); } catch (e) { end(1); err.push(String(e)); return; }
    c.stdout.on("data", (d) => out.push(d));
    c.stderr.on("data", (d) => err.push(d));
    c.on("exit", (code) => end(Number(code) || 0));
    c.on("error", (e) => { if (!done) { err.push(String(e && e.message || e)); end((e && e.exitCode) || 1); } });
  });
}
const canSpawn = () => !!(globalThis.ksu && typeof globalThis.ksu.spawn === "function");
export async function xr(sub, ...args) {
  if (!isKsuWebui()) return { errno: 1, stdout: "", stderr: "需在管理器中打开" };
  const cmd = xrCmd(sub, ...args);
  try { return canSpawn() ? await runSpawn(cmd) : await exec(cmd); }
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

// 乐观更新：先改本地状态并立即重绘（开关、文字当帧生效），xr set 在后台跑；失败再回滚并提示。
// 保存后不立刻拉 status-json，而是合并到稍后的一次刷新（连点几个开关也只刷新一次）
// 本地改动覆盖层：保存中、以及保存完成前就已发出的 status-json 返回的旧值，都不能把开关「弹回去」
const edits = new Map();   // key -> { v, doneAt }（doneAt=null 表示 xr set 还在跑）
function overlay(d) {
  if (!d || !d.config) return;
  for (const [k, e] of edits) d.config[k] = e.v;
}
async function setKey(key, value) {
  if (!SET_KEYS.includes(key)) return false;
  const e = { v: value, doneAt: null };
  edits.set(key, e);
  if (status) { overlay(status); render(status); }
  const r = await xr("set", key, value);
  const res = lastJson(r.stdout);
  if (!res || !res.ok) {
    if (edits.get(key) === e) edits.delete(key);
    snack((res && res.error) || r.stderr || "未保存", true);
    refreshSoon(0);
    return false;
  }
  e.doneAt = Date.now();
  if (res.restart) snack("重启虚拟机后生效");
  refreshSoon();
  return true;
}

// ---- 渲染 ----
function renderPower(d) {
  const v = vmView(d, pending);
  cls($("power-wrap"), "power-wrap " + v.phase);
  cls($("stage"), "stage " + v.phase);   // 舞台底色随阶段：停止 / 失败普通，启动中深色日志，运行中电路板
  txt($("power-text"), v.text);
  cls($("power-text"), "power-text " + v.phase);
  prop($("power"), "disabled", v.phase === "starting" || v.phase === "stopping" || !!(d.vm && d.vm.tripped));
  const steps = $("power-steps");
  hide(steps, v.phase !== "starting");
  if (!steps.hidden) {
    steps.querySelectorAll("[data-step]").forEach((s) => {
      const i = Number(s.dataset.step);
      cls(s, i < (v.step || 0) ? "done" : i === v.step ? "cur" : "");
    });
  }
  if (v.phase === "starting") bootOn(); else bootOff();
  cls($("chip"), "chip " + (v.chip === "运行中" ? "chip-ok" : v.chip === "异常" ? "chip-bad" : "chip-off"));
  txt($("chip-text"), v.chip);
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
  hide($("trip"), !tripped);

  const as = $("sw-autostart");
  prop(as, "selected", cfg.AUTOSTART === "1");
  prop(as, "disabled", tripped && cfg.AUTOSTART !== "1");
  tog($("row-autostart"), "disabled", as.disabled);

  // 首页之外的页面不可见（content-visibility: hidden），其中的文字也照样只在变化时写
  txt($("v-uptime"), vm.running ? fmtUptime(vm.uptime_s) : "—");
  const kind = vm.running ? upKind(net.up_if) : "—";
  txt($("v-up"), kind);
  const ic = kind === "Wi-Fi" ? "wifi" : kind === "蜂窝" ? "signal_cellular_alt" : "cloud_off";
  if ($("i-up").dataset.icon !== ic) setIcon($("i-up"), ic);

  txt($("v-vpn"), VPN_TXT[vpn.status] || "未连接");
  const routes = vpn.routes_active || [];
  txt($("v-routes"), routes.length ? routes.join("\n") : "无");

  seg($("seg-mode"), MODE_OPTS, cfg.XR_MODE || "global", pickMode);
  fillSelect($("sel-access"), ACCESS_OPTS, cfg.XR_EXPOSE || "lan");
  const v6 = $("sw-ipv6");
  if (!ipv6Busy) prop(v6, "selected", d.ipv6 === true);
  prop(v6, "disabled", ipv6Busy || d.ipv6 === null || d.ipv6 === undefined);
  tog($("row-ipv6"), "disabled", v6.disabled);

  prop($("sw-share"), "selected", cfg.VM_SHARE === "1");
  seg($("seg-share"), SHARE_OPTS, cfg.VM_SHARE_MODE || "ro", (v) => setKey("VM_SHARE_MODE", v), sh.rw_supported ? [] : ["rw"]);
  txt($("v-share-path"), displayPath(cfg.VM_SHARE_PATH));

  seg($("seg-notify"), NOTIFY_OPTS, cfg.XR_NOTIFY_TRAFFIC || "total", (v) => setKey("XR_NOTIFY_TRAFFIC", v));
  prop($("sw-debug"), "selected", cfg.XR_DEBUG === "1");
  txt($("btn-export-text"), cfg.XR_DEBUG === "1" ? "诊断导出" : "日志导出");
  prop($("sw-hotspot"), "selected", cfg.XR_HOTSPOT_BRIDGE !== "0");
  txt($("v-hotspot"), net.hotspot ? `已接管 ${net.hotspot}` : "");
  txt($("v-hs-net"), cfg.XR_HOTSPOT_SUBNET || "跟随安卓");
  txt($("v-hs-net-cur"), net.hotspot && net.hotspot_net ? `当前 ${net.hotspot_net}` : "");
  tog($("row-hs-net"), "disabled", cfg.XR_HOTSPOT_BRIDGE === "0");
  fillSelect($("sel-debug-hours"), DEBUG_HOURS_OPTS, cfg.XR_DEBUG_HOURS || "24");

  fillSelect($("sel-cpu"), CPU_OPTS, cfg.VM_CPUS || "2");
  fillSelect($("sel-mem"), MEM_OPTS, cfg.VM_MEM_MB || "768");
  txt($("v-disk"), fmtSize(cfg.VM_DISK_SIZE_MB));
  prop($("btn-expand"), "disabled", !!vm.running);
  prop($("btn-reset"), "disabled", !!vm.running);
}

// 状态刷新单飞：同一时刻最多一个 status-json 在途，期间再要求刷新只记一次，结束后补一次
let refreshing = null; let refreshAgain = false; let soonTimer = 0;
function refresh() {
  if (gone) return Promise.resolve();
  if (refreshing) { refreshAgain = true; return refreshing; }
  refreshing = (async () => {
    do { refreshAgain = false; await refreshOnce(); } while (refreshAgain);
  })().finally(() => { refreshing = null; });
  return refreshing;
}
function refreshSoon(ms = 1200) {
  clearTimeout(soonTimer);
  soonTimer = setTimeout(() => { refresh(); }, ms);
}
async function refreshOnce() {
  const t0 = Date.now();
  const r = await xr("status-json");
  if (gone) return;
  let d = null;
  try { d = JSON.parse(r.stdout); } catch { d = null; }
  if (!d) {
    cls($("chip"), "chip chip-bad");
    txt($("chip-text"), "异常");
    return;
  }
  // 保存完成之后才发出的刷新是权威结果，撤掉对应覆盖；更早的刷新仍套用本地值
  for (const [k, e] of edits) if (e.doneAt !== null && t0 > e.doneAt) edits.delete(k);
  overlay(d);
  render(d);
}

function schedule() {
  clearTimeout(timer);
  if (gone) return;
  const v = status ? vmView(status, pending) : { phase: "stopped" };
  const fast = v.phase === "stopping";
  timer = setTimeout(async () => {
    // 开机中由 bootTick 驱动状态刷新，这里不再并行拉 status-json
    if (!document.hidden && !boot) { await refresh(); if (page === 2) loadTraffic(); }
    schedule();
  }, fast ? 1500 : 5000);
}

// ---- 开机日志：增量读取（按字节偏移），约 200ms 一次。点击启动后电源按钮隐藏，舞台里显示瀑布式日志；
// 启动完成或失败（阶段不再是 starting）即撤掉日志、恢复按钮 ----
// 固定行池：BOOT_ROWS 个行元素一次建好，之后只改文字（不增删节点）。
// 平滑瀑布：新到的行先进缓冲区（积压），每帧按 bootStep 滚动一小段（可为小数行）：
//   - 整数部分变化时才改写行池文字（每帧最多一次）；
//   - 小数部分用 translate3d 把整块下移，最新一行从底部渐隐区慢慢升起；
//   - 积压滚完就停 rAF，没有空转帧；全程不读布局。
const BOOT_ROWS = 16;
const BOOT_LH = 14;      // 与 .boot-log 行高一致
let bootRows = null;
function bootPool() {
  if (bootRows) return bootRows;
  const box = $("boot-log");
  bootRows = Array.from({ length: BOOT_ROWS }, () => document.createElement("div"));
  box.replaceChildren(...bootRows);
  return bootRows;
}

function bootOn() {
  if (boot) return;
  // 点击启动：开机日志会被 start-bg 清空，从当前末尾起跟随，清空后自动从 0 读；
  // 打开页面时已在开机：直接给出已有内容（最多末尾一段）
  boot = { m: -1, c: pending === "start" ? -1 : 0, buf: newBootBuf(), timer: 0, raf: 0, shown: 0, pos: 0, st: undefined, at: Date.now() };
  if (pending !== "start") boot.m = 0;
  bootPool().forEach((r) => { r.textContent = ""; });
  $("boot-log").style.transform = "";
  $("boot").hidden = false;
  bootTick(boot);
}

function bootOff() {
  if (!boot) return;
  clearTimeout(boot.timer);
  if (boot.raf) cancelAnimationFrame(boot.raf);
  boot = null;
  $("boot").hidden = true;
}

function bootPaint(b) {
  if (!b.raf && page === 0) b.raf = requestAnimationFrame(() => bootFrame(b));
}
function bootFrame(b) {
  b.raf = 0;
  if (boot !== b || page !== 0) return;
  const target = b.buf.total;
  b.pos = bootStep(b.pos, target, BOOT_ROWS);
  const top = Math.ceil(b.pos);
  if (top !== b.shown) {
    const rows = bootPool();
    const lines = b.buf.lines;
    const base = target - lines.length;      // 缓冲区第一行的累计序号
    for (let i = 0; i < BOOT_ROWS; i++) {
      const n = top - BOOT_ROWS + i;
      const t = n >= base && n >= 0 ? (lines[n - base] || "") : "";
      if (rows[i].textContent !== t) rows[i].textContent = t;
    }
    b.shown = top;
  }
  const off = (top - b.pos) * BOOT_LH;
  $("boot-log").style.transform = off > 0.01 ? `translate3d(0,${off.toFixed(2)}px,0)` : "";
  if (b.pos < target) b.raf = requestAnimationFrame(() => bootFrame(b));
}

// 开机期间的唯一轮询：约 200ms 一次，上一次 exec 返回后才排下一次（永不重叠）；
// boot-tail 在手机侧走快速路径，只读新增字节。状态（status-json）只在虚拟机状态变化或每 5 秒拉一次，
// 其他定时刷新在开机期间全部暂停，WebView 主线程基本空闲
const BOOT_POLL = 200;
async function bootTick(b) {
  if (boot !== b) return;
  if (!document.hidden) {
    const r = await xr("boot-tail", String(b.m), String(b.c));
    if (boot !== b) return;
    const t = parseBootTail(r.stdout);
    if (t) {
      b.m = t.mod.size; b.c = t.con.size;
      const a = feedBoot(b.buf, "mod", t.mod.text);
      const c = feedBoot(b.buf, "con", t.con.text);
      if (a || c) bootPaint(b);
      if (pending === "start" && bgArmed && t.rc !== null && t.rc !== 0) {
        // 后台 xr start 已失败退出：立刻结束，显示原因
        pending = null;
        await refresh();
        if (!(status && status.vm && (status.vm.running || status.vm.error))) snack("启动失败", true);
        schedule();
        return;
      }
      const now = Date.now();
      if (t.st !== b.st || now - b.at > 5000) {
        b.st = t.st; b.at = now;
        await refresh();          // 可能结束开机阶段（bootOff）
        if (boot !== b) { schedule(); return; }
      }
    }
  }
  if (boot === b) b.timer = setTimeout(() => bootTick(b), BOOT_POLL);
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

// 固定热点网段：前端先做格式预检，冲突（上游 / xr0 网段）由手机侧校验后返回原因
async function editHotspotNet() {
  if (status && status.config && status.config.XR_HOTSPOT_BRIDGE === "0") return;
  const input = $("in-hsnet"); const err = $("hsnet-err");
  const cur = (status && status.config && status.config.XR_HOTSPOT_SUBNET) || "";
  input.value = cur; err.textContent = "";
  for (;;) {
    if (!await dialogResult($("dlg-hsnet"))) return;
    const c = hsSubnetCheck(input.value);
    if (!c.ok) { err.textContent = c.error; continue; }
    if (c.value === cur) return;
    setKey("XR_HOTSPOT_SUBNET", c.value);
    return;
  }
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

// ---- 立即卸载：先换成静态「已卸载」页（停掉轮询、开机日志与电流动画），再让手机侧脱离会话去删 ----
async function uninstallNow() {
  if (!await confirmDlg("立即卸载 XRouter",
    "将停止虚拟机并删除全部数据：OpenWrt 系统盘、设置、VPN 配置与日志，模块随即从管理器列表中消失，无需重启。已导出到 Download 的日志 / 诊断包会保留。此操作不可撤销。",
    "卸载", true)) return;
  gone = true;
  clearTimeout(timer); clearTimeout(soonTimer);
  if (boot) { cancelAnimationFrame(boot.raf); boot = null; }
  document.querySelector(".shell").remove();
  $("gone").hidden = false;
  const r = await xr("uninstall-now", "confirm");
  if (r.errno !== 0) {
    const e = (lastJson(r.stdout) || {}).error;
    $("gone-text").textContent = `卸载请求失败${e ? `（${e}）` : ""}，请到管理器的模块列表中卸载。`;
    $("gone").classList.add("bad");
    setIcon(document.querySelector("#gone md-icon"), "error");
  }
}

// ---- 累计流量：只在设置页可见时读取 ----
async function loadTraffic() {
  const r = await xr("traffic");
  if (gone) return;
  $("v-traffic").textContent = trafficLine(lastJson(r.stdout), "total");
}
async function resetTraffic() {
  if (!await confirmDlg("清零流量统计", "通知与此处的累计流量将从 0 重新计算", "清零", true)) return;
  const r = await xr("traffic-reset");
  const res = lastJson(r.stdout);
  snack(res && res.ok ? "已清零" : "清零失败", !(res && res.ok));
  loadTraffic();
}

// ---- 日志导出：日志统一，只分普通 / 调试。调试模式关闭时导出普通日志，开启时导出完整诊断快照 ----
let exportBusy = false;
function exportMode() { return status && status.config && status.config.XR_DEBUG === "1" ? "diag" : "log"; }
async function exportLogs() {
  if (exportBusy) return;
  const diag = exportMode() === "diag";
  exportBusy = true; $("btn-export").disabled = true;
  snack(diag ? "正在收集诊断信息…" : "正在打包日志…");
  const r = diag ? await xr("diag-export") : await xr("log-export");
  const res = lastJson(r.stdout);
  exportBusy = false; $("btn-export").disabled = false;
  snack(res && res.ok ? `已保存到 Download/${String(res.path).split("/").pop()}` : "导出失败", !(res && res.ok));
}

// ---- 页面切换：标题栏固定，只切换内容；点击 Tab 或左右滑动，均瞬时切换、不做任何过渡 ----
// 关掉 Material Tab 指示条的 250ms 滑动动画：激活态由 CSS :host([active]) 直接显示
Tab.prototype[ANIMATE_INDICATOR] = function () {};

function goPage(i) {
  i = Math.max(0, Math.min(2, i));
  const pages = document.querySelectorAll("#pager > .page");
  pages.forEach((p, k) => p.classList.toggle("on", k === i));
  const tabs = $("tabs");
  if (tabs.activeTabIndex !== i) tabs.activeTabIndex = i;
  if (page === i) return;
  page = i;
  if (i === 0 && boot) bootPaint(boot);   // 回到首页：补上不在首页时攒下的日志
  // 先让切页这一帧画出来，再去读流量（不和切页抢同一帧）
  if (i === 2) requestAnimationFrame(() => setTimeout(loadTraffic, 0));
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

// 运行中的电流走线：启动时按 pcb.js 一次性建好（8 个元素），之后只靠 CSS 动画，不再碰 DOM
function buildFlow() {
  $("pcb-flow").replaceChildren(...PCB_FLOW.map((f) => {
    const d = document.createElement("span");
    d.className = "pf";
    d.style.cssText = `left:${f.x}px;top:${f.y}px;width:${f.len}px;transform:rotate(${f.deg}deg);--t:${f.delay}s`;
    d.append(document.createElement("i"));
    return d;
  }));
}

function bind() {
  document.querySelectorAll("md-icon[data-icon]").forEach(icon);
  buildFlow();
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
  $("btn-export").addEventListener("click", exportLogs);
  $("row-hs-net").addEventListener("click", editHotspotNet);
  $("hsnet-clear").addEventListener("click", () => { $("in-hsnet").value = ""; $("hsnet-err").textContent = ""; });
  $("btn-luci").addEventListener("click", () => xr("luci"));
  $("btn-import").addEventListener("click", importVpn);
  $("btn-share-path").addEventListener("click", pickSharePath);
  $("sel-access").addEventListener("change", (e) => setKey("XR_EXPOSE", e.target.value));
  $("sel-cpu").addEventListener("change", (e) => setKey("VM_CPUS", e.target.value));
  $("sel-mem").addEventListener("change", (e) => setKey("VM_MEM_MB", e.target.value));
  $("btn-expand").addEventListener("click", expandDisk);
  $("btn-reset").addEventListener("click", resetDisk);
  $("btn-traffic-reset").addEventListener("click", resetTraffic);
  $("btn-uninstall").addEventListener("click", uninstallNow);
  document.addEventListener("visibilitychange", () => {
    document.documentElement.classList.toggle("hid", document.hidden);   // 不可见时暂停电流动画
    if (!document.hidden && !boot && !gone) { refresh(); schedule(); }
  });
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
