// ==============================================================================
// x-route v2 WebUI（docs/v2-plan/10-cli-webui.md）
// 只通过 ksu.exec 调用 scripts/xr，不直接读写配置文件。
// ==============================================================================
"use strict";

const XR = "sh /data/adb/modules/x_route/scripts/xr";
const SET_KEYS = [
  "XR_EXPOSE", "VPN_MODE", "VPN_ROUTES", "VM_SHARE", "VM_SHARE_MODE", "VM_SHARE_PATH",
  "AUTOSTART", "AUTOSTART_DELAY_SEC", "VM_CPUS", "VM_MEM_MB", "VM_SWIOTLB_MB",
  "READY_TIMEOUT_SEC", "STABLE_SEC", "VM_NET", "VM_DISK_RW",
];
const ARG_RE = /^[A-Za-z0-9_\-]+$/;
let status = null;
let seq = 0;
let timer = null;

const $ = (id) => document.getElementById(id);
const hasKsu = () => typeof window.ksu !== "undefined" && typeof window.ksu.exec === "function";

// 单引号包裹，值里的 ' 转义为 '\''
function shq(v) {
  return "'" + String(v).replace(/'/g, "'\\''") + "'";
}

function exec(cmd) {
  return new Promise((resolve) => {
    if (!hasKsu()) {
      resolve({ errno: 1, stdout: "", stderr: "不在 KernelSU 管理器中" });
      return;
    }
    const cb = `__xr_cb_${Date.now()}_${++seq}`;
    window[cb] = (errno, stdout, stderr) => {
      delete window[cb];
      resolve({ errno: typeof errno === "number" ? errno : 0, stdout: stdout || "", stderr: stderr || "" });
    };
    try {
      window.ksu.exec(cmd, "{}", cb);
    } catch (e) {
      delete window[cb];
      resolve({ errno: 1, stdout: "", stderr: String(e) });
    }
  });
}

// xr 子命令：子命令与非值参数都要过白名单
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

async function setKey(key, value) {
  if (!SET_KEYS.includes(key)) return false;
  const r = await xr("set", key, value);
  let res = null;
  try { res = JSON.parse(r.stdout.trim().split("\n").pop()); } catch (e) { res = null; }
  if (!res || !res.ok) {
    toast(`保存失败：${(res && res.error) || r.stderr || "未知错误"}`, true);
    await refresh();
    return false;
  }
  toast(res.restart ? "已保存，需重启 VM 生效" : "已保存");
  await refresh();
  return true;
}

// ---- CIDR 前端校验（与 common.sh 同规则：格式、私网、主机位为 0）----
function ip2int(s) {
  const p = s.split(".").map(Number);
  if (p.length !== 4 || p.some((x) => !Number.isInteger(x) || x < 0 || x > 255)) return null;
  return ((p[0] << 24) >>> 0) + (p[1] << 16) + (p[2] << 8) + p[3];
}
function cidrOk(c) {
  const m = /^(\d{1,3}(?:\.\d{1,3}){3})\/(\d{1,2})$/.exec(c);
  if (!m) return "格式错误";
  const ip = ip2int(m[1]); const pfx = Number(m[2]);
  if (ip === null || pfx < 8 || pfx > 32) return "格式错误";
  const mask = pfx === 0 ? 0 : (0xffffffff << (32 - pfx)) >>> 0;
  if (((ip & mask) >>> 0) !== ip) return "主机位不为 0";
  const a = ip >>> 24; const b = (ip >>> 16) & 255;
  const priv = (a === 10 && pfx >= 8) || (a === 172 && b >= 16 && b <= 31 && pfx >= 12) ||
    (a === 192 && b === 168 && pfx >= 16) || (a === 100 && b >= 64 && b <= 127 && pfx >= 10);
  return priv ? "" : "不是私网地址";
}

function fmtUptime(s) {
  s = Number(s) || 0;
  const h = Math.floor(s / 3600); const m = Math.floor((s % 3600) / 60);
  return h ? `${h} 小时 ${m} 分` : `${m} 分 ${s % 60} 秒`;
}

const STATE_TXT = { stopped: "已停止", booting: "启动中", ready: "已就绪", stable: "已稳定" };

function render(d) {
  const vm = d.vm || {}; const net = d.net || {}; const vpn = d.vpn || {}; const sh = d.share || {}; const cfg = d.config || {};
  const pill = $("vm-pill");
  pill.textContent = STATE_TXT[vm.state] || vm.state || "-";
  pill.className = "pill " + (vm.running ? (vm.state === "stable" ? "pill-ok" : "pill-warn") : "pill-off");
  $("st-vm").textContent = vm.running ? `运行中（pid ${vm.pid}，就绪通道 ${vm.ready_channel || "-"}）` : "已停止";
  $("st-uptime").textContent = vm.running ? fmtUptime(vm.uptime_s) : "-";
  $("st-netmode").textContent = net.mode === "ndc" ? "ndc（netd 本地网络）" : net.mode === "fallback" ? "退回模式（table 97）" : "未启用";
  $("st-up").textContent = net.up_if || "-";
  $("st-expose").textContent = (net.expose_if && net.expose_if.length) ? net.expose_if.join(", ") : "无";
  $("st-vpn").textContent = `${vpn.mode || "-"}，生效 ${(vpn.routes_active || []).length} 条`;
  $("st-share").textContent = sh.enabled ? `开启（本次 ${sh.mode}${sh.ce_ready ? "" : "，手机未解锁"}）` : "关闭";

  // 熔断
  const tripped = vm.tripped !== null && vm.tripped !== undefined;
  $("trip-bar").classList.toggle("hidden", !tripped);
  if (tripped) $("trip-time").textContent = vm.tripped;
  $("btn-start").disabled = tripped || vm.running;
  $("btn-stop").disabled = !vm.running;
  $("btn-expand").disabled = vm.running;
  $("btn-reset").disabled = vm.running;

  // 访问链接
  const pre = cfg.XR_NET_PREFIX || "172.29.233";
  $("lnk-luci").href = $("lnk-luci").textContent = `http://${pre}.254`;
  $("lnk-ttyd").href = $("lnk-ttyd").textContent = `http://${pre}.254:7681`;
  $("txt-ssh").textContent = `ssh root@${pre}.254`;
  $("txt-route").textContent = `${pre}.0/24 via <手机 IP>`;

  // 表单（正在编辑的控件不覆盖）
  document.querySelectorAll("[data-key]").forEach((el) => {
    if (el === document.activeElement) return;
    const v = cfg[el.dataset.key];
    if (v === undefined) return;
    if (el.type === "checkbox") el.checked = v === "1"; else el.value = v;
  });
  if (document.activeElement !== $("ta-routes")) {
    $("ta-routes").value = (cfg.VPN_ROUTES || "").split(",").filter(Boolean).join("\n");
  }
  $("vpn-active").textContent = (vpn.routes_active || []).join(", ") || "无";
  $("vpn-rejected").textContent = (vpn.routes_rejected || []).join(", ") || "无";
  $("hint-all").classList.toggle("hidden", cfg.XR_EXPOSE !== "all");

  $("opt-rw").disabled = !sh.rw_supported;
  $("hint-norw").classList.toggle("hidden", !!sh.rw_supported);

  const as = $("cb-autostart"); const ah = $("hint-autostart");
  let why = "";
  if (tripped) why = "存在未确认的熔断，不能开启自启。";
  else if (!vm.last_good_match) why = "需先用当前配置手动启动成功一次（运行到“已稳定”），才能开启自启。";
  as.disabled = !!why && cfg.AUTOSTART !== "1";
  ah.textContent = why;
  ah.classList.toggle("hidden", !why);
}

async function refresh() {
  const r = await xr("status-json");
  try {
    status = JSON.parse(r.stdout);
    render(status);
    $("env-badge").textContent = "已连接";
    $("env-badge").className = "badge badge-ok";
  } catch (e) {
    $("env-badge").textContent = hasKsu() ? "读取状态失败" : "需在 KernelSU 管理器中打开";
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
  btn.disabled = true;
  const r = await xr(sub, ...args);
  if (r.errno === 0) toast(okMsg);
  else toast(`失败：${(r.stdout + r.stderr).trim().split("\n").pop() || r.errno}`, true);
  await refresh();
}

function bind() {
  $("btn-start").onclick = (e) => action(e.target, "start", [], "已启动，等待就绪");
  $("btn-stop").onclick = (e) => action(e.target, "stop", [], "已停止");
  $("btn-ack").onclick = (e) => action(e.target, "ack-trip", [], "熔断已确认");
  $("btn-expand").onclick = (e) => {
    const mb = $("in-expand").value.trim();
    if (!/^\d+$/.test(mb) || Number(mb) < 512 || Number(mb) > 16384) { toast("请输入 512–16384 之间的 MB 数", true); return; }
    action(e.target, "expand", [mb], `系统盘已扩容到 ${mb} MB，下次开机生效`);
  };
  $("btn-reset").onclick = (e) => {
    if (!confirm("重置系统盘会清空 VM 内的全部数据和设置，确定继续？")) return;
    if (!confirm("再次确认：VM 内数据将全部丢失。")) return;
    action(e.target, "reset-disk", [], "系统盘已重置");
  };
  $("btn-log").onclick = async () => {
    const which = $("sel-log").value;
    if (!["xr", "console", "ctl", "vmm"].includes(which)) return;
    const r = await xr("log", which, "200");
    const box = $("log-box");
    box.textContent = r.stdout || "（空）";
    box.classList.remove("hidden");
    box.scrollTop = box.scrollHeight;
  };
  $("btn-routes").onclick = () => {
    const lines = $("ta-routes").value.split(/[\s,]+/).map((s) => s.trim()).filter(Boolean);
    if (lines.length > 64) { toast("最多 64 条", true); return; }
    for (const c of lines) {
      const err = cidrOk(c);
      if (err) { toast(`${c}：${err}`, true); return; }
    }
    setKey("VPN_ROUTES", lines.join(","));
  };
  document.querySelectorAll("[data-key]").forEach((el) => {
    el.addEventListener("change", () => {
      const v = el.type === "checkbox" ? (el.checked ? "1" : "0") : el.value.trim();
      setKey(el.dataset.key, v);
    });
  });
  document.addEventListener("visibilitychange", () => {
    if (document.hidden) stopPolling(); else { refresh(); startPolling(); }
  });
}

bind();
refresh();
startPolling();
