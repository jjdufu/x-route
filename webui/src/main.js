// XRouter WebUI：只通过 kernelsu exec 调用 xr 命令行
import "@material/web/tabs/tabs.js";
import "@material/web/tabs/primary-tab.js";
import { Tab, ANIMATE_INDICATOR } from "@material/web/tabs/internal/tab.js";
import { EASING } from "@material/web/internal/motion/animation.js";
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
  SET_KEYS, MODE_OPTS, NOTIFY_OPTS, DEBUG_HOURS_OPTS, ACCESS_OPTS, SHARE_OPTS, CPU_OPTS, MEM_OPTS,
  xrCmd, lastJson, upKind, fmtUptime, vmView, displayPath, fmtSize, expandSizes,
  parseBootTail, newBootBuf, feedBoot, bootStep, trafficLine, hsSubnetCheck, vpnTunnels, splitLines,
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
let vpnBusy = null;       // enable | disable
let vpnBusyAt = 0;
let vpnBusyFrom = "";
let chipTok = 0;
let chipRestPhase = null;
let chipLock = null;      // heal | shutdown | fail

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
  if (res.restart) snack("重启服务后生效");
  refreshSoon();
  return true;
}


// ---- 芯片过渡（docs/chip-transition-flow.md）：只用 transform / opacity / stroke-dashoffset ----
const CHIP_REDUCE = window.matchMedia("(prefers-reduced-motion: reduce)");
const EASE_DECEL = (EASING && EASING.EMPHASIZED_DECELERATE) || "cubic-bezier(0.05, 0.7, 0.1, 1)";
const EASE_ACCEL = (EASING && EASING.EMPHASIZED_ACCELERATE) || "cubic-bezier(0.3, 0, 0.8, 0.15)";
const EASE_STD = (EASING && EASING.STANDARD) || "cubic-bezier(0.2, 0, 0, 1)";
const prefersReduce = () => CHIP_REDUCE.matches;

function finishTree(el) {
  if (!el) return;
  el.getAnimations({ subtree: true }).forEach((a) => { try { a.finish(); } catch { try { a.cancel(); } catch { /* 已结束 */ } } });
}

function fragRest(assembled) {
  const svg = document.querySelector(".chip-svg");
  if (!svg) return;
  svg.querySelectorAll(".frag").forEach((g) => {
    g.style.transform = assembled
      ? "translate(0, 0) rotate(0deg)"
      : `translate(${g.dataset.dx || 0}px, ${g.dataset.dy || 0}px) rotate(${g.dataset.rot || 0}deg)`;
  });
  const cracks = svg.querySelector(".crack-lines");
  const gap = svg.querySelector(".crack-gap");
  if (cracks) {
    const len = cracks.getTotalLength ? cracks.getTotalLength() : 400;
    cracks.style.strokeDasharray = String(len);
    cracks.style.strokeDashoffset = assembled ? String(len) : "0";
    cracks.style.opacity = assembled ? "0" : "1";
  }
  if (gap) gap.style.opacity = assembled ? "0" : "1";
}

function clearFlowInline(svg) {
  if (!svg) return;
  svg.querySelectorAll(".chip-flow").forEach((p) => {
    p.style.animation = "";
    p.style.strokeDasharray = "";
    p.style.strokeDashoffset = "";
  });
  const intact = svg.querySelector(".chip-intact");
  const cracked = svg.querySelector(".chip-cracked");
  if (intact) { intact.style.opacity = ""; intact.style.visibility = ""; }
  if (cracked) { cracked.style.opacity = ""; cracked.style.visibility = ""; }
  const red = svg.querySelector(".traces-red");
  if (red) red.style.opacity = "";
}

function chipJump(phase) {
  chipTok += 1;
  chipLock = null;
  const wrap = $("power-wrap");
  finishTree(wrap);
  finishTree($("boot"));
  fragRest(phase === "running");
  clearFlowInline(wrap && wrap.querySelector(".chip-svg"));
  if (phase !== "starting") bootOff();
  if (wrap) wrap.className = "power-wrap " + phase;
  chipRestPhase = phase;
}

function waitAnims(anims) {
  return Promise.all(anims.filter(Boolean).map((a) => a.finished.catch(() => {})));
}

async function bootCollapse(live) {
  const el = $("boot");
  if (boot) {
    clearTimeout(boot.timer);
    if (boot.raf) cancelAnimationFrame(boot.raf);
    boot = null;
  }
  if (!el) return;
  if (!prefersReduce() && el.classList.contains("on") && live()) {
    const a = el.animate(
      [{ opacity: 1, transform: "none" }, { opacity: 0, transform: "translateY(-28px) scale(0.86)" }],
      { duration: 300, easing: EASE_ACCEL, fill: "forwards" }
    );
    await a.finished.catch(() => {});
    try { a.cancel(); } catch { /* */ }
  }
  el.style.transition = "none";
  el.classList.remove("on");
  el.style.transform = "";
  el.style.opacity = "";
  requestAnimationFrame(() => { el.style.transition = ""; });
  clearTimeout(bootClear);
  bootClear = setTimeout(() => {
    if (!boot) { bootPool().forEach((r) => { r.textContent = ""; }); $("boot-log").style.transform = ""; }
  }, 40);
}

async function chipToRunning(from, live) {
  if (from !== "starting") { chipJump("running"); return; }
  await bootCollapse(live);
  if (!live()) return;
  if (prefersReduce()) { chipJump("running"); return; }
  const wrap = $("power-wrap");
  wrap.className = "power-wrap running healing";
  fragRest(false);
  const svg = wrap.querySelector(".chip-svg");
  const frags = [...svg.querySelectorAll(".frag")];
  const cracks = svg.querySelector(".crack-lines");
  const gap = svg.querySelector(".crack-gap");
  const len = cracks && cracks.getTotalLength ? cracks.getTotalLength() : 400;
  if (cracks) { cracks.style.strokeDasharray = String(len); cracks.style.strokeDashoffset = "0"; cracks.style.opacity = "1"; }
  const anims = frags.map((g) => g.animate(
    [
      { transform: `translate(${g.dataset.dx || 0}px, ${g.dataset.dy || 0}px) rotate(${g.dataset.rot || 0}deg)` },
      { transform: "translate(0, 0) rotate(0deg)" },
    ],
    { duration: 500, easing: EASE_DECEL, fill: "forwards" }
  ));
  if (cracks) anims.push(cracks.animate(
    [{ strokeDashoffset: 0, opacity: 1 }, { strokeDashoffset: len, opacity: 0 }],
    { duration: 500, easing: EASE_DECEL, fill: "forwards" }
  ));
  if (gap) anims.push(gap.animate([{ opacity: 1 }, { opacity: 0 }], { duration: 500, easing: EASE_DECEL, fill: "forwards" }));
  await waitAnims(anims);
  if (!live()) return;
  const intact = svg.querySelector(".chip-intact");
  const cracked = svg.querySelector(".chip-cracked");
  const flows = [...svg.querySelectorAll(".chip-flow")];
  flows.forEach((p) => { p.style.animation = "none"; p.style.strokeDasharray = "100"; p.style.strokeDashoffset = "100"; });
  intact.style.visibility = "visible";
  const fade = [
    intact.animate([{ opacity: 0 }, { opacity: 1 }], { duration: 400, easing: EASE_STD, fill: "forwards" }),
    cracked.animate([{ opacity: 1 }, { opacity: 0 }], { duration: 400, easing: EASE_STD, fill: "forwards" }),
  ];
  flows.forEach((p, i) => {
    fade.push(p.animate(
      [{ strokeDashoffset: 100 }, { strokeDashoffset: 0 }],
      { duration: 400, delay: Math.min(280, (i % 12) * 24), easing: EASE_STD, fill: "forwards" }
    ));
  });
  await waitAnims(fade);
  if (!live()) return;
  finishTree(wrap);
  clearFlowInline(svg);
  fragRest(true);
  chipLock = null;
  wrap.className = "power-wrap running";
  chipRestPhase = "running";
}

async function chipToStopped(from, live) {
  if (from !== "running" && from !== "stopping") { chipJump("stopped"); return; }
  const wrap = $("power-wrap");
  wrap.className = "power-wrap stopping shutdown";
  if (prefersReduce()) { chipJump("stopped"); return; }
  const svg = wrap.querySelector(".chip-svg");
  const flows = [...svg.querySelectorAll(".chip-flow")];
  const shrink = flows.map((p, i) => {
    p.style.animation = "none";
    p.style.strokeDasharray = "100";
    return p.animate(
      [{ strokeDashoffset: 0 }, { strokeDashoffset: 100 }],
      { duration: 400, delay: Math.min(200, (11 - (i % 12)) * 16), easing: EASE_STD, fill: "forwards" }
    );
  });
  await waitAnims(shrink);
  if (!live()) return;
  const intact = svg.querySelector(".chip-intact");
  const cracked = svg.querySelector(".chip-cracked");
  fragRest(true);
  const cracks = svg.querySelector(".crack-lines");
  const gap = svg.querySelector(".crack-gap");
  const len = cracks && cracks.getTotalLength ? cracks.getTotalLength() : 400;
  if (cracks) { cracks.style.strokeDasharray = String(len); cracks.style.strokeDashoffset = String(len); cracks.style.opacity = "0"; }
  cracked.style.visibility = "visible";
  const open = [
    intact.animate([{ opacity: 1 }, { opacity: 0 }], { duration: 500, easing: EASE_DECEL, fill: "forwards" }),
    cracked.animate([{ opacity: 0 }, { opacity: 1 }], { duration: 200, easing: EASE_DECEL, fill: "forwards" }),
  ];
  if (cracks) open.push(cracks.animate(
    [{ strokeDashoffset: len, opacity: 0 }, { strokeDashoffset: 0, opacity: 1 }],
    { duration: 500, easing: EASE_DECEL, fill: "forwards" }
  ));
  if (gap) open.push(gap.animate([{ opacity: 0 }, { opacity: 1 }], { duration: 500, easing: EASE_DECEL, fill: "forwards" }));
  svg.querySelectorAll(".frag").forEach((g) => {
    open.push(g.animate(
      [
        { transform: "translate(0, 0) rotate(0deg)" },
        { transform: `translate(${g.dataset.dx || 0}px, ${g.dataset.dy || 0}px) rotate(${g.dataset.rot || 0}deg)` },
      ],
      { duration: 500, easing: EASE_DECEL, fill: "forwards" }
    ));
  });
  await waitAnims(open);
  if (!live()) return;
  finishTree(wrap);
  clearFlowInline(svg);
  fragRest(false);
  chipLock = null;
  wrap.className = "power-wrap stopped";
  chipRestPhase = "stopped";
}

async function chipToFailed(from, live) {
  if (from === "starting") {
    const el = $("boot");
    if (boot) { clearTimeout(boot.timer); if (boot.raf) cancelAnimationFrame(boot.raf); boot = null; }
    if (!prefersReduce() && el.classList.contains("on") && live()) {
      const a = el.animate([{ opacity: 1 }, { opacity: 0 }], { duration: 180, easing: EASE_ACCEL, fill: "forwards" });
      await a.finished.catch(() => {});
      try { a.cancel(); } catch { /* */ }
    }
    el.style.transition = "none";
    el.classList.remove("on");
    el.style.opacity = "";
    requestAnimationFrame(() => { el.style.transition = ""; });
  }
  if (!live()) return;
  const wrap = $("power-wrap");
  wrap.className = "power-wrap failed";
  fragRest(false);
  if (prefersReduce()) { chipJump("failed"); return; }
  const red = wrap.querySelector(".traces-red");
  if (red && live()) {
    const a = red.animate(
      [{ opacity: 1 }, { opacity: 0.35 }, { opacity: 1 }, { opacity: 0.35 }, { opacity: 1 }],
      { duration: 360, easing: EASE_STD }
    );
    await a.finished.catch(() => {});
  }
  if (!live()) return;
  chipLock = null;
  chipRestPhase = "failed";
}

function chipFollow(phase) {
  if (chipRestPhase === null) { chipJump(phase); return; }
  if (phase === chipRestPhase && !chipLock) return;
  const tok = ++chipTok;
  const live = () => tok === chipTok;
  if (prefersReduce()) { chipJump(phase); return; }
  if (phase === "starting") { chipJump("starting"); return; }
  if (phase === "running") {
    if (chipRestPhase === "starting") chipLock = "heal";
    chipToRunning(chipRestPhase, live);
    return;
  }
  if (phase === "stopping" || phase === "stopped") {
    if (chipLock === "shutdown") return;
    if (chipRestPhase === "stopped" && phase === "stopped") return;
    chipLock = "shutdown";
    chipToStopped(chipRestPhase, live);
    return;
  }
  if (phase === "failed") {
    chipLock = "fail";
    chipToFailed(chipRestPhase, live);
    return;
  }
  chipJump(phase);
}

// ---- 渲染 ----
function renderPower(d) {
  const v = vmView(d, pending);
  const wrapCls = chipLock === "heal" ? "power-wrap running healing"
    : chipLock === "shutdown" ? "power-wrap stopping shutdown"
    : "power-wrap " + v.phase;
  cls($("power-wrap"), wrapCls);
  cls($("stage"), "stage " + v.phase);   // 舞台随阶段：停止 / 失败红色断电，启动中变暗叠开机日志，运行中绿色 + 电流
  cls($("bg"), "bg " + v.phase + (page ? " dim" : ""));   // 整页背景同步变色；组网、设置页更弱
  prop($("power"), "disabled", v.phase === "starting" || v.phase === "stopping" || !!(d.vm && d.vm.tripped) || !!chipLock);
  const tripped = d.vm && d.vm.tripped !== null && d.vm.tripped !== undefined;
  const err = $("power-err");
  // 失败细节叠在舞台内，不占文档流；整机复位已有 #trip 卡片，这里不再重复
  if (v.phase === "failed" && v.text && !tripped) { txt(err, v.text); hide(err, false); }
  else { hide(err, true); txt(err, ""); }
  if (v.phase === "starting") bootOn();
  chipFollow(v.phase);
  if (v.phase !== "starting" && !chipLock) bootOff();
  cls($("chip"), "chip " + (v.chip === "运行中" ? "chip-ok" : v.chip === "异常" ? "chip-bad" : "chip-off"));
  txt($("chip-text"), v.chip);
  tog(document.querySelector(".shell"), "nostate", false);   // 拿到状态（或上次缓存）才显示按钮与背景，不先闪「已停止」
}

// ---- 组网：隧道列表。每条隧道一张卡片（模板克隆一次，之后只改文字）；只有一条时与原单卡片布局相同：
// 标题 OpenVPN + 连接状态 + 路由网段，导入按钮在卡片底部。多条时每张卡片标题为隧道名、状态缩成标题右侧的小标记，
// 显示隧道地址与各自同步的网段；导入按钮单独放在列表下方 ----
const vpnCards = new Map();   // key -> 卡片元素
function vpnCard(key) {
  let c = vpnCards.get(key);
  if (!c) {
    c = $("tpl-vpn").content.firstElementChild.cloneNode(true);
    c.querySelectorAll("md-icon[data-icon]").forEach(icon);
    vpnCards.set(key, c);
  }
  return c;
}
function renderVpn(vpn) {
  const list = vpnTunnels(vpn);
  const multi = list.length > 1;
  const box = $("vpn-list");
  const cards = list.map((t) => {
    const c = vpnCard(t.key);
    tog(c, "multi", multi);
    txt(c.querySelector(".vpn-name"), multi ? t.name : "OpenVPN");
    txt(c.querySelector(".vpn-st"), t.text);
    cls(c.querySelector(".vpn-st"), "val vpn-st " + t.status);
    cls(c.querySelector(".vpn-badge"), "vpn-badge " + t.status);
    txt(c.querySelector(".vpn-badge-text"), t.text);
    txt(c.querySelector(".vpn-addr"), t.addr || "—");
    txt(c.querySelector(".vpn-sync"), multi && t.routes.length ? "自动同步" : "");
    txt(c.querySelector(".vpn-routes"), t.routes.length ? t.routes.join("\n") : "无");
    return c;
  });
  const acts = $("vpn-actions");
  if (!multi) cards[0].append(acts);
  const want = multi ? [...cards, acts] : cards;
  if (box.children.length !== want.length || want.some((c, i) => box.children[i] !== c)) box.replaceChildren(...want);
  for (const k of vpnCards.keys()) if (!list.some((t) => t.key === k)) vpnCards.delete(k);
  if (vpnBusy && Date.now() - vpnBusyAt > 20000) {
    snack(vpnBusy === "enable" ? "启动超时" : "停止超时", true);
    vpnBusy = null;
  }
  if (vpnBusy && vpn) {
    const stNow = vpn.status || "down";
    if (vpnBusy === "enable" && stNow !== "down") vpnBusy = null;
    if (vpnBusy === "disable" && stNow === "down") vpnBusy = null;
  }
  const configured = !!(vpn && vpn.configured);
  const vmOn = !!(status && status.vm && status.vm.running);
  const st = (vpn && vpn.status) || "down";
  $("btn-vpn-start").disabled = !(configured && vmOn && !vpnBusy && st === "down");
  $("btn-vpn-stop").disabled = !(configured && vmOn && !vpnBusy && (st === "up" || st === "connecting"));
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

  renderVpn(vpn);

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
  // 运行中系统盘灰显（不设 disabled：点了要能提示「停止后可修改」）
  txt($("v-disk-sub"), vm.running ? "停止后可修改" : "");
  tog($("row-disk"), "soft-off", !!vm.running);
  tog($("disk-actions"), "soft-off", !!vm.running);
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
  saveCache(r.stdout);
}

// 上次的状态缓存：重新打开模块页面时先按它画按钮与背景，等 status-json 返回再更正（不先闪「已停止」）
const CACHE_KEY = "xr.status";
function saveCache(raw) {
  try { localStorage.setItem(CACHE_KEY, JSON.stringify({ t: Date.now(), raw })); } catch { /* 存储不可用 */ }
}
function loadCache() {
  try {
    const c = JSON.parse(localStorage.getItem(CACHE_KEY) || "null");
    if (!c || Date.now() - c.t > 6 * 3600e3) return null;
    return JSON.parse(c.raw);
  } catch { return null; }
}

function schedule() {
  clearTimeout(timer);
  if (gone) return;
  const v = status ? vmView(status, pending) : { phase: "stopped" };
  const fast = v.phase === "stopping" || !!vpnBusy;
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
  clearTimeout(bootClear);
  bootPool().forEach((r) => { r.textContent = ""; });
  $("boot-log").style.transform = "";
  tog($("boot"), "on", true);   // CSS：按钮先淡出，日志随后淡入（交叉淡入淡出约 200ms）
  bootTick(boot);
}

// 结束开机阶段：日志淡出、按钮淡入；淡出结束后再清空行池（淡出过程中文字保持不动）
let bootClear = 0;
function bootOff() {
  if (!boot) return;
  clearTimeout(boot.timer);
  if (boot.raf) cancelAnimationFrame(boot.raf);
  boot = null;
  tog($("boot"), "on", false);
  clearTimeout(bootClear);
  bootClear = setTimeout(() => { if (!boot) { bootPool().forEach((r) => { r.textContent = ""; }); $("boot-log").style.transform = ""; } }, 260);
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
    if (!await confirmDlg("停止服务", "", "停止", true)) return;
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

async function actVpn(kind) {
  if (vpnBusy) return;
  vpnBusy = kind;
  vpnBusyAt = Date.now();
  vpnBusyFrom = (status && status.vpn && status.vpn.status) || "down";
  renderVpn((status && status.vpn) || {});
  const r = kind === "enable" ? await xr("vpn-enable") : await xr("vpn-disable");
  const res = lastJson(r.stdout);
  if (!res || !res.ok) {
    vpnBusy = null;
    snack((res && res.error) || (kind === "enable" ? "启动失败" : "停止失败"), true);
    await refresh();
    return;
  }
  snack(kind === "enable" ? "已启动" : "已停止");
  await refresh();
  schedule();
}

async function pickSharePath() {
  const cur = status && status.config ? status.config.VM_SHARE_PATH : "";
  const path = await openBrowser({ mode: "dir", start: cur, xr });
  if (path) setKey("VM_SHARE_PATH", path);
}

// 热点网关地址：前端先做格式预检，冲突（上游 / xr0 网段）由手机侧校验后返回原因
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

// 运行中系统盘不可改：按钮灰显但仍可点，点了只提示
function diskLocked() {
  if (status && status.vm && status.vm.running) { snack("停止后可修改"); return true; }
  return false;
}

// CPU / 内存：重启虚拟机才生效，保存前先确认；取消则把下拉框恢复原值
async function pickRestartKey(key, sel, label, fmt) {
  const cur = (status && status.config && status.config[key]) || "";
  const v = sel.value;
  if (v === cur) return;
  const running = !!(status && status.vm && status.vm.running);
  const ok = await confirmDlg(`${label}改为 ${fmt(v)}`, running ? "需要重启服务后生效" : "下次启动时生效", "保存");
  if (!ok) { sel.select(cur); return; }
  setKey(key, v);
}

async function expandDisk() {
  if (diskLocked()) return;
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
  if (diskLocked()) return;
  if (!await confirmDlg("重置系统盘", "OpenWrt 内的数据将全部清除", "重置", true)) return;
  const r = await xr("reset-disk");
  snack(r.errno === 0 ? "已重置" : "重置失败", r.errno !== 0);
  await refresh();
}

// ---- 立即卸载：先换成静态「已卸载」页（停掉轮询、开机日志与电流动画），再让手机侧脱离会话去删 ----
async function uninstallNow() {
  if (!await confirmDlg("立即卸载 XRouter",
    "将停止服务并删除全部数据：OpenWrt 系统盘、设置、VPN 配置与日志，模块随即从管理器列表中消失，无需重启。已导出到 Download 的日志 / 诊断包会保留。此操作不可撤销。",
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

// ---- 累计流量弹窗：手机本机 / 热点各三项（总流量 | 公网 | 内网），由 OpenWrt 分来源统计 ----
let splitBusy = false;
async function openTraffic() {
  if (splitBusy) return;
  splitBusy = true;
  for (const id of ["tr-ph-total", "tr-ph-wan", "tr-ph-lan", "tr-hs-total", "tr-hs-wan", "tr-hs-lan"]) txt($(id), "…");
  txt($("tr-note"), "");
  const shown = dialogResult($("dlg-traffic"));
  const r = await xr("traffic-split");
  splitBusy = false;
  const res = lastJson(r.stdout);
  const l = splitLines(res);
  if (!l) {
    for (const id of ["tr-ph-total", "tr-ph-wan", "tr-ph-lan", "tr-hs-total", "tr-hs-wan", "tr-hs-lan"]) txt($(id), "—");
    txt($("tr-note"), (res && res.error) || "读取失败");
  } else {
    txt($("tr-ph-total"), l.phone.total); txt($("tr-ph-wan"), l.phone.wan); txt($("tr-ph-lan"), l.phone.lan);
    txt($("tr-hs-total"), l.hotspot.total); txt($("tr-hs-wan"), l.hotspot.wan); txt($("tr-hs-lan"), l.hotspot.lan);
    txt($("tr-note"), "本次运行以来，经 OpenWrt 转发的流量");
  }
  await shown;
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

// ---- 页面切换：标题栏固定，只切换内容；点击 Tab 或左右滑动都有短而干脆的横移淡入（180ms，ease-out）。
// 只动新页（WAAPI，transform + opacity，走合成层），旧页立即隐藏，不重叠、不跟手、不吸附；点击当帧就切 ----
// Material Tab 指示条：默认 250ms，缩短为 180ms，与页面同步
const PAGE_MS = 180;
Tab.prototype[ANIMATE_INDICATOR] = function (previousTab) {
  if (!this.indicator) return;
  this.indicator.getAnimations().forEach((a) => a.cancel());
  const frames = this.getKeyframes(previousTab);
  if (frames !== null) this.indicator.animate(frames, { duration: PAGE_MS, easing: EASING.EMPHASIZED_DECELERATE || "cubic-bezier(.2,.8,.2,1)" });
};

function goPage(i) {
  i = Math.max(0, Math.min(2, i));
  const pages = document.querySelectorAll("#pager > .page");
  pages.forEach((p, k) => p.classList.toggle("on", k === i));
  const tabs = $("tabs");
  if (tabs.activeTabIndex !== i) tabs.activeTabIndex = i;
  if (page === i) return;
  const dir = i > page ? 1 : -1;
  page = i;
  pages[i].animate([{ opacity: 0, transform: `translate3d(${dir * 28}px,0,0)` }, { opacity: 1, transform: "none" }],
    { duration: PAGE_MS, easing: "cubic-bezier(.2,.8,.2,1)" });
  tog($("bg"), "dim", i !== 0);
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
  $("btn-vpn-start").addEventListener("click", () => actVpn("enable"));
  $("btn-vpn-stop").addEventListener("click", () => actVpn("disable"));
  $("btn-share-path").addEventListener("click", pickSharePath);
  $("sel-access").addEventListener("change", (e) => setKey("XR_EXPOSE", e.target.value));
  $("sel-cpu").addEventListener("change", (e) => pickRestartKey("VM_CPUS", e.target, "CPU ", (v) => `${v} 核`));
  $("sel-mem").addEventListener("change", (e) => pickRestartKey("VM_MEM_MB", e.target, "内存", fmtSize));
  $("row-traffic").addEventListener("click", openTraffic);
  $("btn-project").addEventListener("click", () => xr("project"));
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
  const cached = loadCache();
  if (cached) render(cached);
  document.querySelector(".shell").removeAttribute("unresolved");
  await refresh();
  schedule();
})();
