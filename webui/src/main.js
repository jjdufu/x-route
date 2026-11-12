// XRouter WebUI — Google Material Design 3 (M3 Navigation Bar + M3 Expressive Surfaces)
import "@material/web/labs/navigationbar/navigation-bar.js";
import "@material/web/labs/navigationtab/navigation-tab.js";
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
import "@material/web/textfield/outlined-text-field.js";
import { exec, spawn, isKsuWebui } from "kernelsu-alt";
import "./style.css";
import { ICONS } from "./icons.js";
import {
  SET_KEYS, MODE_OPTS, NOTIFY_OPTS, DEBUG_HOURS_OPTS, ACCESS_OPTS, SHARE_OPTS, CPU_OPTS, MEM_OPTS,
  VPN_TXT, labelOf, xrCmd, lastJson, upKind, fmtUptime, vmView, displayPath, fmtSize, expandSizes,
  trafficLine, hsSubnetCheck, vpnProfiles, splitLines,
} from "./logic.js";
import { openBrowser } from "./file-browser.js";

const $ = (id) => document.getElementById(id);
// 只在值真的变化时写 DOM：5 秒一次的状态刷新大多数时候什么都不改，不触发样式重算与重排
const txt = (el, v) => { v = String(v); if (el.textContent !== v) el.textContent = v; };
const cls = (el, v) => { if (el.className !== v) el.className = v; };
const prop = (el, k, v) => { if (el && el[k] !== v) el[k] = v; };
const hide = (el, v) => { if (el.hidden !== v) el.hidden = v; };
const tog = (el, c, v) => { if (el.classList.contains(c) !== v) el.classList.toggle(c, v); };

const REDUCE_MOTION = window.matchMedia("(prefers-reduced-motion: reduce)");
const DARK_MEDIA = window.matchMedia("(prefers-color-scheme: dark)");
const prefersReduce = () => REDUCE_MOTION.matches;

const MODE_DESC = {
  global: "接管本机与热点全部流量",
  bypass: "仅作局域网网关，不影响本机",
};

let status = null;
let pending = null;      // start | stop
let pendingAt = 0;       // 发出操作的时间，用于兜底超时
let ipv6Busy = false;
let page = 0;
let timer = null;
let gone = false;        // 已请求立即卸载：停止一切轮询
let vpnBusy = null;      // { id, kind, at }
let themeMode = "auto";  // auto | light | dark

export function icon(el) {
  el.textContent = ICONS[el.dataset.icon] || "";
}
export function setIcon(el, name) {
  if (el.dataset.icon === name && el.textContent) return;
  el.dataset.icon = name;
  icon(el);
}

// ---- 明暗主题（默认跟随系统，支持顶栏一键切换） ----
const THEME_KEY = "xr.theme";
const THEME_CYCLE = ["auto", "light", "dark"];
const THEME_NAMES = { auto: "跟随系统", light: "浅色模式", dark: "深色模式" };
const THEME_ICONS = { auto: "brightness_auto", light: "light_mode", dark: "dark_mode" };

function syncMetaTheme() {
  const dark = themeMode === "dark" || (themeMode === "auto" && DARK_MEDIA.matches);
  const meta = $("meta-theme");
  if (meta) meta.content = dark ? "#111318" : "#f8fafd";
}

function applyTheme(mode, notify) {
  themeMode = THEME_CYCLE.includes(mode) ? mode : "auto";
  if (themeMode === "auto") delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = themeMode;
  syncMetaTheme();
  const ic = $("i-theme");
  if (ic) setIcon(ic, THEME_ICONS[themeMode]);
  try { localStorage.setItem(THEME_KEY, themeMode); } catch { /* 忽略存储异常 */ }
  if (notify) snack(`外观：${THEME_NAMES[themeMode]}`);
}

function loadTheme() {
  let saved = "auto";
  try { saved = localStorage.getItem(THEME_KEY) || "auto"; } catch { saved = "auto"; }
  applyTheme(saved, false);
}

function cycleTheme() {
  const next = THEME_CYCLE[(THEME_CYCLE.indexOf(themeMode) + 1) % THEME_CYCLE.length];
  applyTheme(next, true);
}

// ---- 命令通道：优先 ksu.spawn（后台线程异步回调，不堵 WebView 主线程），旧环境回退 exec ----
function runSpawn(cmd) {
  return new Promise((resolve) => {
    const out = []; const err = []; let done = false;
    const end = (code) => { if (!done) { done = true; resolve({ errno: code, stdout: out.join("\n"), stderr: err.join("\n") }); } };
    let c;
    try { c = spawn(cmd); } catch (e) { end(1); err.push(String(e)); return; }
    c.stdout.on("data", (d) => out.push(d));
    c.stderr.on("data", (d) => err.push(d));
    c.on("exit", (code) => end(Number(code) || 0));
    c.on("error", (e) => { if (!done) { err.push(String((e && e.message) || e)); end((e && e.exitCode) || 1); } });
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

// ---- M3 分段药丸与模式选择卡 ----
function seg(box, opts, current, onPick, disabled) {
  const sig = JSON.stringify([current, disabled || []]);
  if (box.dataset.sig === sig) return;
  box.dataset.sig = sig;
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

function renderModeCards(box, opts, current, onPick) {
  if (box.dataset.sig === current) return;
  box.dataset.sig = current;
  box.replaceChildren(...opts.map((o) => {
    const on = o.value === current;
    const card = document.createElement("button");
    card.type = "button";
    card.role = "radio";
    card.setAttribute("aria-checked", String(on));
    card.className = "mode-card" + (on ? " on" : "");
    card.innerHTML = "<md-ripple></md-ripple>";

    const top = document.createElement("div");
    top.className = "mode-card-top";
    const title = document.createElement("span");
    title.className = "mode-card-title";
    title.textContent = o.label;
    const chk = document.createElement("md-icon");
    chk.className = "mode-check filled";
    setIcon(chk, "check_circle");
    top.append(title, chk);

    const desc = document.createElement("span");
    desc.className = "mode-card-desc";
    desc.textContent = MODE_DESC[o.value] || "";

    card.append(top, desc);
    card.onclick = () => { if (!on) onPick(o.value); };
    return card;
  }));
}

function fillSelect(sel, opts, value) {
  if (!sel.dataset.filled) {
    sel.replaceChildren(...opts.map((o) => {
      const it = document.createElement("md-select-option");
      it.value = o.value;
      const h = document.createElement("div");
      h.slot = "headline";
      h.textContent = o.label;
      it.append(h);
      return it;
    }));
    sel.dataset.filled = "1";
  }
  if (sel.value === value || sel.matches(":focus-within")) return;
  sel.querySelectorAll("md-select-option").forEach((o) => { o.selected = o.value === value; });
  sel.updateComplete.then(() => { if (sel.value !== value) sel.select(value); });
}

// ---- 乐观更新：先改本地状态并立即重绘，xr set 在后台跑；失败再回滚并提示 ----
const edits = new Map();   // key -> { v, doneAt }
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

// ---- 渲染 ----
function renderPower(d) {
  const v = vmView(d, pending);
  const cfg = d.config || {};
  const tripped = d.vm && d.vm.tripped !== null && d.vm.tripped !== undefined;

  cls($("hero-card"), "hero-card " + v.phase);

  const chipCls = v.phase === "running" ? "chip chip-ok"
    : (v.phase === "starting" || v.phase === "stopping") ? "chip chip-busy"
    : v.phase === "failed" ? "chip chip-bad" : "chip";
  cls($("chip"), chipCls);
  txt($("chip-text"), v.chip);

  const heroIcon = $("hero-icon");
  const heroIconName = v.phase === "running" ? "check_circle"
    : (v.phase === "starting" || v.phase === "stopping") ? "bolt"
    : v.phase === "failed" ? "error" : "power_settings_new";
  setIcon(heroIcon, heroIconName);
  tog(heroIcon, "filled", v.phase !== "stopped");

  const modeLabel = labelOf(MODE_OPTS, cfg.XR_MODE || "global") || "全局接管";
  const gwHost = cfg.XR_NET_PREFIX ? `${cfg.XR_NET_PREFIX}.254` : "";
  if (v.phase === "running") {
    txt($("power-title"), "运行中");
    txt($("power-sub"), `${modeLabel}${gwHost ? ` · 网关 ${gwHost}` : " · OpenWrt 路由已就绪"}`);
  } else if (v.phase === "starting") {
    txt($("power-title"), v.text);
    txt($("power-sub"), (v.step || 0) >= 1 ? "正在配置策略路由与网关转发…" : "正在引导 OpenWrt 路由系统…");
  } else if (v.phase === "stopping") {
    txt($("power-title"), "正在停止");
    txt($("power-sub"), "正在安全停止服务并恢复本机网络…");
  } else if (v.phase === "failed") {
    txt($("power-title"), "异常");
    txt($("power-sub"), tripped ? "检测到连续异常复位，请先确认上方提示" : "路由服务未能就绪，可检查配置后重试");
  } else {
    txt($("power-title"), "已停止");
    txt($("power-sub"), "点击下方按钮启动 OpenWrt 路由服务");
  }

  const err = $("power-err");
  if (v.phase === "failed" && v.text && !tripped) {
    txt(err, v.text);
    hide(err, false);
  } else {
    hide(err, true);
    txt(err, "");
  }

  const inTransition = v.phase === "starting" || v.phase === "stopping";
  hide($("hero-progress"), !inTransition);
  if (inTransition) {
    const curStep = v.phase === "stopping" ? -1 : (v.step ?? 0);
    $("boot-steps").querySelectorAll(".step-pill").forEach((el) => {
      const s = Number(el.dataset.step);
      tog(el, "on", s === curStep);
      tog(el, "done", s < curStep);
    });
  }

  prop($("power"), "disabled", inTransition || !!tripped);
  setIcon($("power-icon"), v.phase === "running" ? "stop" : v.phase === "failed" ? "refresh" : "play_arrow");
  txt($("power-btn-text"),
    v.phase === "running" ? "停止服务"
      : v.phase === "starting" ? "正在启动…"
      : v.phase === "stopping" ? "正在停止…"
      : v.phase === "failed" ? "重新启动" : "启动服务");
  prop($("btn-luci"), "disabled", v.phase !== "running");

  tog(document.querySelector(".shell"), "nostate", false);
}

// ---- 组网：多配置卡片列表渲染（导入即展示，点击对应配置直接启动 / 停止 / 删除） ----
const vpnCards = new Map();
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
  const vmOn = !!(status && status.vm && status.vm.running);
  const list = vpnProfiles(vpn);

  if (vpnBusy && Date.now() - vpnBusy.at > 20000) {
    snack(vpnBusy.kind === "enable" ? "启动超时" : vpnBusy.kind === "disable" ? "停止超时" : "操作超时", true);
    vpnBusy = null;
  }
  if (vpnBusy) {
    const target = list.find((p) => p.id === vpnBusy.id);
    if (vpnBusy.kind === "delete" && !target) vpnBusy = null;
    else if (vpnBusy.kind === "enable" && target && (target.status !== "down" || target.enabled)) vpnBusy = null;
    else if (vpnBusy.kind === "disable" && (!target || (target.status === "down" && !target.enabled))) vpnBusy = null;
  }

  hide($("vpn-empty"), list.length > 0);
  const box = $("vpn-list");
  hide(box, list.length === 0);

  const cards = list.map((p) => {
    const c = vpnCard(p.key);
    const rowBusy = !!(vpnBusy && vpnBusy.id === p.id);
    const visSt = rowBusy && vpnBusy.kind === "enable" ? "connecting" : p.status;
    const visTxt = rowBusy
      ? (vpnBusy.kind === "enable" ? "正在启动" : vpnBusy.kind === "disable" ? "正在停止" : "正在删除")
      : p.text;
    const activeOrEnabled = p.status === "up" || p.status === "connecting" || (vmOn && p.enabled);

    cls(c, "vpn-card " + visSt);
    tog(c.querySelector(".vpn-ico md-icon"), "filled", visSt === "up" || visSt === "connecting");
    txt(c.querySelector(".vpn-name"), p.name);
    txt(c.querySelector(".vpn-dev-sub"),
      !p.managed ? `外部隧道${p.dev ? ` · ${p.dev}` : ""}`
        : rowBusy ? `${visTxt}…`
        : p.dev ? `接口 ${p.dev}`
        : !vmOn ? "需先在「状态」页启动路由服务"
        : p.enabled && p.status === "down" ? "已启用 · 等待重连"
        : "已就绪 · 点击启动即可连接");

    cls(c.querySelector(".vpn-badge"), "vpn-badge " + visSt);
    txt(c.querySelector(".vpn-badge-text"), visTxt);
    cls(c.querySelector(".vpn-st"), "val vpn-st " + visSt);
    txt(c.querySelector(".vpn-st"), visTxt);

    hide(c.querySelector(".vpn-addr-row"), !p.addr && p.status !== "up");
    txt(c.querySelector(".vpn-addr"), p.addr || "—");
    txt(c.querySelector(".vpn-sync"), p.routes.length ? "已自动同步至本机" : "");
    txt(c.querySelector(".vpn-routes"), p.routes.length ? p.routes.join("\n") : "无");

    const actBox = c.querySelector(".vpn-card-actions");
    hide(actBox, !p.managed);
    if (p.managed) {
      const btnStart = c.querySelector(".btn-vpn-start");
      const btnStop = c.querySelector(".btn-vpn-stop");
      const btnDel = c.querySelector(".btn-vpn-del");
      hide(btnStart, activeOrEnabled);
      hide(btnStop, !activeOrEnabled);
      prop(btnStart, "disabled", !vmOn || !!vpnBusy);
      prop(btnStop, "disabled", !vmOn || !!vpnBusy);
      hide(btnDel, !p.id);
      prop(btnDel, "disabled", !!vpnBusy);
      btnStart.onclick = () => actVpn("enable", p.id, p.name);
      btnStop.onclick = () => actVpn("disable", p.id, p.name);
      btnDel.onclick = () => delVpn(p.id, p.name);
    }
    return c;
  });

  if (box.children.length !== cards.length || cards.some((c, i) => box.children[i] !== c)) {
    box.replaceChildren(...cards);
  }
  for (const k of vpnCards.keys()) if (!list.some((t) => t.key === k)) vpnCards.delete(k);

  const upList = list.filter((p) => p.status === "up");
  const connList = list.filter((p) => p.status === "connecting");
  const st = upList.length ? "up" : connList.length ? "connecting" : ((vpn && vpn.status) || "down");
  const configured = list.length > 0 || !!(vpn && vpn.configured);
  const activeCount = Array.isArray(vpn && vpn.routes_active) ? vpn.routes_active.length : 0;

  txt($("vpn-summary"),
    !configured ? "导入配置文件后，点击对应配置即可运行或停止"
      : !vmOn ? `已导入 ${list.length} 份配置 · 请先在「状态」页启动路由服务`
      : upList.length ? `已连接 ${upList.length}/${list.length} 份配置${activeCount ? ` · 已同步 ${activeCount} 个网段` : ""}`
      : connList.length ? `正在连接「${connList[0].name}」…`
      : `已导入 ${list.length} 份配置 · 点击对应配置上的「启动」即可连接`);

  // 底部导航栏与首页磁贴同步显示组网状态
  prop($("nav-vpn"), "showBadge", st === "up");
  txt($("v-home-vpn"), !configured ? "未导入" : upList.length === 1 ? upList[0].name : upList.length > 1 ? `${upList.length} 条已连接` : (VPN_TXT[st] || "未连接"));
  txt($("v-home-routes"), activeCount ? `已同步 ${activeCount} 个网段` : configured ? `共 ${list.length} 份配置` : "点击导入配置");
}

function render(d) {
  status = d;
  const vm = d.vm || {};
  const net = d.net || {};
  const vpn = d.vpn || {};
  const sh = d.share || {};
  const cfg = d.config || {};

  if (pending === "start" && (vm.running || vm.error)) pending = vm.running ? null : pending;
  if (pending === "stop" && !vm.running) pending = null;
  if (pending && Date.now() - pendingAt > (pending === "start" ? 120000 : 45000)) {
    snack(pending === "start" ? "启动超时" : "停止超时", true);
    pending = null;
  }

  renderPower(d);

  const tripped = vm.tripped !== null && vm.tripped !== undefined;
  hide($("trip"), !tripped);

  const as = $("sw-autostart");
  const asOn = cfg.AUTOSTART === "1";
  prop(as, "selected", asOn);
  prop(as, "disabled", tripped && !asOn);
  tog($("row-autostart"), "disabled", as.disabled);
  txt($("v-as-st"), asOn ? "已开启 · 开机自动拉起路由服务" : "已关闭 · 需手动启动路由服务");

  // 首页双列指标磁贴
  txt($("v-uptime"), vm.running ? fmtUptime(vm.uptime_s) : "—");
  const v = vmView(d, pending);
  const runMap = { running: "运行正常", starting: "正在启动", stopping: "正在停止", failed: "服务异常" };
  txt($("v-run-st"), runMap[v.phase] || "已停止");

  const kind = vm.running ? upKind(net.up_if) : "—";
  txt($("v-up"), kind);
  txt($("v-up-sub"), vm.running ? (net.up_if ? `接口 ${net.up_if}` : "未检测到上游") : "服务未运行");
  const ic = kind === "Wi-Fi" ? "wifi" : kind === "蜂窝" ? "signal_cellular_alt" : "cloud_off";
  setIcon($("i-up"), ic);

  const curMode = cfg.XR_MODE || "global";
  txt($("v-home-mode"), labelOf(MODE_OPTS, curMode) || "全局接管");
  txt($("v-home-hs"), net.hotspot ? `DHCP 已接管 ${net.hotspot}` : cfg.XR_HOTSPOT_BRIDGE !== "0" ? "热点自动接管已开启" : "热点接管已关闭");

  renderVpn(vpn);

  // 网络页
  renderModeCards($("seg-mode"), MODE_OPTS, curMode, pickMode);
  fillSelect($("sel-access"), ACCESS_OPTS, cfg.XR_EXPOSE || "lan");
  const v6 = $("sw-ipv6");
  if (!ipv6Busy) prop(v6, "selected", d.ipv6 === true);
  prop(v6, "disabled", ipv6Busy || d.ipv6 === null || d.ipv6 === undefined);
  tog($("row-ipv6"), "disabled", v6.disabled);

  prop($("sw-hotspot"), "selected", cfg.XR_HOTSPOT_BRIDGE !== "0");
  txt($("v-hotspot"), net.hotspot ? `已接管 ${net.hotspot}` : "由路由系统接管手机热点地址分配");
  txt($("v-hs-net"), cfg.XR_HOTSPOT_SUBNET || "跟随安卓");
  txt($("v-hs-net-cur"), net.hotspot && net.hotspot_net ? `当前 ${net.hotspot_net}` : "");
  tog($("row-hs-net"), "disabled", cfg.XR_HOTSPOT_BRIDGE === "0");

  seg($("seg-notify"), NOTIFY_OPTS, cfg.XR_NOTIFY_TRAFFIC || "total", (val) => setKey("XR_NOTIFY_TRAFFIC", val));

  // 系统页
  prop($("sw-share"), "selected", cfg.VM_SHARE === "1");
  seg($("seg-share"), SHARE_OPTS, cfg.VM_SHARE_MODE || "ro", (val) => setKey("VM_SHARE_MODE", val), sh.rw_supported ? [] : ["rw"]);
  txt($("v-share-path"), displayPath(cfg.VM_SHARE_PATH));

  fillSelect($("sel-cpu"), CPU_OPTS, cfg.VM_CPUS || "2");
  fillSelect($("sel-mem"), MEM_OPTS, cfg.VM_MEM_MB || "768");
  txt($("v-disk"), fmtSize(cfg.VM_DISK_SIZE_MB));
  txt($("v-disk-sub"), vm.running ? "停止后可修改" : "离线状态下可扩容或重置");
  tog($("row-disk"), "soft-off", !!vm.running);
  tog($("disk-actions"), "soft-off", !!vm.running);

  prop($("sw-debug"), "selected", cfg.XR_DEBUG === "1");
  txt($("btn-export-text"), cfg.XR_DEBUG === "1" ? "诊断导出" : "日志导出");
  fillSelect($("sel-debug-hours"), DEBUG_HOURS_OPTS, cfg.XR_DEBUG_HOURS || "24");
}

// ---- 状态刷新单飞与本地缓存 ----
let refreshing = null;
let refreshAgain = false;
let soonTimer = 0;

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
  for (const [k, e] of edits) if (e.doneAt !== null && t0 > e.doneAt) edits.delete(k);
  overlay(d);
  render(d);
  saveCache(r.stdout);
}

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
  const fast = v.phase === "stopping" || v.phase === "starting" || !!vpnBusy;
  timer = setTimeout(async () => {
    if (!document.hidden) {
      await refresh();
      if (page === 1) loadTraffic();
    }
    schedule();
  }, fast ? 1500 : 5000);
}

// ---- 操作 ----
function act(kind) {
  pending = kind;
  pendingAt = Date.now();
  renderPower(status || {});
  schedule();
  (kind === "start" ? xr("start-bg") : xr("stop-bg")).then((r) => {
    const res = lastJson(r.stdout);
    if (r.errno !== 0 || !res || !res.ok) {
      pending = null;
      snack(kind === "start" ? "启动失败" : "停止失败", true);
      refresh().then(schedule);
    }
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
  ipv6Busy = true;
  sw.disabled = true;
  $("row-ipv6").classList.add("busy");
  const r = await xr("ipv6", want);
  const res = lastJson(r.stdout);
  ipv6Busy = false;
  $("row-ipv6").classList.remove("busy");
  if (!res || !res.ok) snack((res && res.error) || "切换失败", true);
  await refresh();
}

async function importVpn() {
  const path = await openBrowser({ mode: "ovpn", xr });
  if (!path) return;
  const r = await xr("vpn-import", path, "0");
  const res = lastJson(r.stdout);
  if (res && res.ok) snack(res.name ? `已导入「${res.name}」` : "已导入配置");
  else snack((res && res.error) || "导入失败", true);
  await refresh();
}

async function actVpn(kind, id, name) {
  if (vpnBusy) return;
  vpnBusy = { id: id || "", kind, at: Date.now() };
  renderVpn((status && status.vpn) || {});
  const r = kind === "enable"
    ? (id ? await xr("vpn-enable", id) : await xr("vpn-enable"))
    : (id ? await xr("vpn-disable", id) : await xr("vpn-disable"));
  const res = lastJson(r.stdout);
  vpnBusy = null;
  if (!res || !res.ok) {
    snack((res && res.error) || (kind === "enable" ? "启动失败" : "停止失败"), true);
    await refresh();
    return;
  }
  snack(kind === "enable" ? `已启动「${name || "配置"}」` : `已停止「${name || "配置"}」`);
  await refresh();
  schedule();
}

async function delVpn(id, name) {
  if (!id || vpnBusy) return;
  if (!await confirmDlg(`删除配置 ${name}`, "删除后若该隧道正在运行将自动断开连接", "删除", true)) return;
  vpnBusy = { id, kind: "delete", at: Date.now() };
  renderVpn((status && status.vpn) || {});
  const r = await xr("vpn-delete", id);
  const res = lastJson(r.stdout);
  vpnBusy = null;
  if (!res || !res.ok) {
    snack((res && res.error) || "删除失败", true);
    await refresh();
    return;
  }
  snack(`已删除「${name}」`);
  await refresh();
}

async function pickSharePath() {
  const cur = status && status.config ? status.config.VM_SHARE_PATH : "";
  const path = await openBrowser({ mode: "dir", start: cur, xr });
  if (path) setKey("VM_SHARE_PATH", path);
}

async function editHotspotNet() {
  if (status && status.config && status.config.XR_HOTSPOT_BRIDGE === "0") return;
  const input = $("in-hsnet");
  const err = $("hsnet-err");
  const cur = (status && status.config && status.config.XR_HOTSPOT_SUBNET) || "";
  input.value = cur;
  input.error = false;
  err.textContent = "";
  for (;;) {
    if (!await dialogResult($("dlg-hsnet"))) return;
    const c = hsSubnetCheck(input.value);
    if (!c.ok) {
      input.error = true;
      err.textContent = c.error;
      continue;
    }
    input.error = false;
    if (c.value === cur) return;
    setKey("XR_HOTSPOT_SUBNET", c.value);
    return;
  }
}

function diskLocked() {
  if (status && status.vm && status.vm.running) { snack("停止后可修改"); return true; }
  return false;
}

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

// ---- 立即卸载：先切换静态页停止一切轮询，再通知后台卸载 ----
async function uninstallNow() {
  if (!await confirmDlg("立即卸载 XRouter",
    "将停止服务并删除全部数据：OpenWrt 系统盘、设置、VPN 配置与日志，模块随即从管理器列表中消失，无需重启。已导出到 Download 的日志 / 诊断包会保留。此操作不可撤销。",
    "卸载", true)) return;
  gone = true;
  clearTimeout(timer);
  clearTimeout(soonTimer);
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

// ---- 累计流量：仅在进入「网络」页时按需读取 ----
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
    txt($("tr-ph-total"), l.phone.total);
    txt($("tr-ph-wan"), l.phone.wan);
    txt($("tr-ph-lan"), l.phone.lan);
    txt($("tr-hs-total"), l.hotspot.total);
    txt($("tr-hs-wan"), l.hotspot.wan);
    txt($("tr-hs-lan"), l.hotspot.lan);
    txt($("tr-note"), "本次运行以来，经 OpenWrt 转发的流量");
  }
  await shown;
}

// ---- 日志与诊断导出 ----
let exportBusy = false;
function exportMode() { return status && status.config && status.config.XR_DEBUG === "1" ? "diag" : "log"; }
async function exportLogs() {
  if (exportBusy) return;
  const diag = exportMode() === "diag";
  exportBusy = true;
  $("btn-export").disabled = true;
  snack(diag ? "正在收集诊断信息…" : "正在打包日志…");
  const r = diag ? await xr("diag-export") : await xr("log-export");
  const res = lastJson(r.stdout);
  exportBusy = false;
  $("btn-export").disabled = false;
  snack(res && res.ok ? `已保存到 Download/${String(res.path).split("/").pop()}` : "导出失败", !(res && res.ok));
}

// ---- M3 页面切换：底部导航栏 + 横向轻扫手势，180ms 空间过渡 ----
const PAGE_MS = 180;
const PAGE_MAX = 3;

function syncAppbarScroll(activePage) {
  tog($("appbar"), "scrolled", !!activePage && activePage.scrollTop > 4);
}

function goPage(i) {
  i = Math.max(0, Math.min(PAGE_MAX, i));
  const pages = document.querySelectorAll("#pager > .page");
  pages.forEach((p, k) => p.classList.toggle("on", k === i));
  const nav = $("nav");
  if (nav && nav.activeIndex !== i) nav.activeIndex = i;
  syncAppbarScroll(pages[i]);
  if (page === i) return;
  const dir = i > page ? 1 : -1;
  page = i;
  if (!prefersReduce()) {
    pages[i].animate(
      [{ opacity: 0, transform: `translate3d(${dir * 24}px,0,0)` }, { opacity: 1, transform: "none" }],
      { duration: PAGE_MS, easing: "cubic-bezier(.2,.8,.2,1)" }
    );
  }
  if (i === 1) requestAnimationFrame(() => setTimeout(loadTraffic, 0));
}

function bindPager() {
  const pager = $("pager");
  const nav = $("nav");
  nav.addEventListener("navigation-bar-activated", (e) => {
    if (e.detail && typeof e.detail.activeIndex === "number") goPage(e.detail.activeIndex);
  });

  document.querySelectorAll("#pager > .page").forEach((p) => {
    p.addEventListener("scroll", () => { if (p.classList.contains("on")) syncAppbarScroll(p); }, { passive: true });
  });

  let sx = 0; let sy = 0; let st = 0; let track = false;
  pager.addEventListener("touchstart", (e) => {
    if (e.touches.length !== 1) { track = false; return; }
    const t = e.touches[0];
    sx = t.clientX; sy = t.clientY; st = e.timeStamp; track = true;
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
    if (e.target !== sw) {
      e.preventDefault();
      sw.selected = !sw.selected;
    }
    onChange();
  });
  sw.addEventListener("click", (e) => e.stopPropagation());
  sw.addEventListener("change", onChange);
}

function bind() {
  loadTheme();
  DARK_MEDIA.addEventListener("change", syncMetaTheme);
  document.querySelectorAll("md-icon[data-icon]").forEach(icon);
  bindPager();
  goPage(0);

  $("btn-theme").addEventListener("click", cycleTheme);
  $("power").addEventListener("click", power);
  $("btn-ack").addEventListener("click", async () => { await xr("ack-trip"); await refresh(); });
  $("tile-mode").addEventListener("click", () => goPage(1));
  $("tile-vpn").addEventListener("click", () => goPage(2));

  bindRowSwitch($("row-autostart"), $("sw-autostart"), () => setKey("AUTOSTART", $("sw-autostart").selected ? "1" : "0"));
  bindRowSwitch($("row-share"), $("sw-share"), () => setKey("VM_SHARE", $("sw-share").selected ? "1" : "0"));
  bindRowSwitch($("row-ipv6"), $("sw-ipv6"), toggleIpv6);
  bindRowSwitch($("row-hotspot"), $("sw-hotspot"), () => setKey("XR_HOTSPOT_BRIDGE", $("sw-hotspot").selected ? "1" : "0"));
  bindRowSwitch($("row-debug"), $("sw-debug"), () => setKey("XR_DEBUG", $("sw-debug").selected ? "1" : "0"));

  $("sel-debug-hours").addEventListener("change", (e) => setKey("XR_DEBUG_HOURS", e.target.value));
  $("btn-export").addEventListener("click", exportLogs);
  $("row-hs-net").addEventListener("click", editHotspotNet);
  $("hsnet-clear").addEventListener("click", () => {
    $("in-hsnet").value = "";
    $("in-hsnet").error = false;
    $("hsnet-err").textContent = "";
  });
  $("btn-luci").addEventListener("click", () => xr("luci"));
  $("btn-import").addEventListener("click", importVpn);
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
    document.documentElement.classList.toggle("hid", document.hidden);
    if (!document.hidden && !gone) { refresh(); schedule(); }
  });
}

// ---- 边到边沉浸式安全区适配 ----
function edgeToEdge() {
  if (!isKsuWebui()) return;
  const l = document.createElement("link");
  l.rel = "stylesheet";
  l.href = "/internal/insets.css";
  document.head.append(l);
  try { if (window.ksu && typeof window.ksu.enableEdgeToEdge === "function") window.ksu.enableEdgeToEdge(true); } catch { /* 旧管理器 */ }
}

function previewStatus(phase) {
  const running = phase === "running";
  const starting = phase === "starting";
  const failed = phase === "failed";
  return {
    vm: {
      running: running || starting,
      state: running ? "stable" : starting ? "booting" : "off",
      uptime_s: running ? 11520 : starting ? 12 : 0,
      error: failed ? "180 秒内未就绪，请检查内核镜像" : null,
      tripped: null,
    },
    net: { gw: running ? "on" : starting ? "off" : "block", up_if: running ? "wlan0" : "", hotspot: running ? "ap0" : "" },
    config: {
      AUTOSTART: "1", XR_MODE: "global", XR_EXPOSE: "lan", XR_NET_PREFIX: "172.28.221",
      VM_SHARE: "0", VM_CPUS: "2", VM_MEM_MB: "768", VM_DISK_SIZE_MB: "1024", XR_HOTSPOT_BRIDGE: "1",
    },
    vpn: {
      configured: true,
      enabled: running,
      status: running ? "up" : "down",
      addr: running ? "10.8.0.5/24" : "",
      routes_active: running ? ["10.8.0.0/24", "192.168.31.0/24"] : [],
      profiles: [
        {
          id: "home_nas",
          name: "家里 NAS",
          enabled: running,
          status: running ? "up" : "down",
          dev: running ? "tun0" : "",
          addr: running ? "10.8.0.5/24" : "",
          mtu: 1420,
          routes: running ? ["10.8.0.0/24", "192.168.31.0/24"] : [],
        },
        {
          id: "office_gw",
          name: "公司办公网",
          enabled: false,
          status: "down",
          dev: "",
          addr: "",
          mtu: 0,
          routes: [],
        },
      ],
    },
    share: { rw_supported: true },
    ipv6: running ? true : null,
  };
}

window.__xrPreviewSet = (phase, pageIdx) => {
  render(previewStatus(phase));
  if (typeof pageIdx === "number") goPage(pageIdx);
};

(async () => {
  edgeToEdge();
  bind();
  await customElements.whenDefined("md-navigation-bar");
  const pv = !isKsuWebui() ? (window.__XR_PREVIEW__ || { phase: "running" }) : null;
  if (pv) {
    render(previewStatus(pv.phase || "running"));
    if (typeof pv.page === "number") goPage(pv.page);
    document.querySelector(".shell").removeAttribute("unresolved");
    return;
  }
  const cached = loadCache();
  if (cached) render(cached);
  document.querySelector(".shell").removeAttribute("unresolved");
  await refresh();
  schedule();
})();
