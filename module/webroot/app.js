// ==============================================================================
// x-route: SuKiSU / KernelSU WebUI Frontend Controller (app.js)
// Uses window.ksu.exec(cmd, options, callback) to interact with module scripts
// ==============================================================================

const MODDIR = "/data/adb/modules/x_route";
const CONF_FILE = "/data/adb/x-route/config/config.conf";
let currentConfig = {
  ROUTER_ENABLED: "1",
  VM_BACKEND: "auto",
  VM_CPUS: "2",
  VM_MEM_MB: "256",
  HOTSPOT_BRIDGE_ENABLED: "1",
  HOST_REDIRECT_ENABLED: "1",
  NAS_MOUNT_ENABLED: "1",
  NAS_HOST_PATH: "/data/media/0/NAS",
  KEEPALIVE_ENABLED: "1",
  KEEPALIVE_PACKAGES: "com.leohao.android.alistlite",
  WAKELOCK_ENABLED: "1",
  BYPASS_CHARGE_ENABLED: "0",
};

let ksuCallbackSeq = 0;

function isKsuEnv() {
  return typeof window.ksu !== "undefined" && typeof window.ksu.exec === "function";
}

function execRoot(cmd) {
  return new Promise((resolve) => {
    if (!isKsuEnv()) {
      resolve({ errno: 0, stdout: mockExec(cmd), stderr: "" });
      return;
    }
    const cbName = `__xroute_cb_${Date.now()}_${++ksuCallbackSeq}`;
    window[cbName] = function (errno, stdout, stderr) {
      delete window[cbName];
      resolve({
        errno: typeof errno === "number" ? errno : 0,
        stdout: stdout || "",
        stderr: stderr || "",
      });
    };
    try {
      window.ksu.exec(cmd, "{}", cbName);
    } catch (e) {
      delete window[cbName];
      resolve({ errno: 1, stdout: "", stderr: String(e) });
    }
  });
}

function mockExec(cmd) {
  if (cmd.includes("action.sh status-json")) {
    return JSON.stringify({
      vm: { running: true, pid: "14820", backend: "gunyah", cpus: 2, mem_mb: 256 },
      net: {
        cell_if: "rmnet_data0",
        ap_if: "ap0",
        host_redirect: "active",
        gw_online: true,
        lan_gw: "192.168.100.1",
        lan_clients: 2,
      },
      keepalive: {
        daemon_running: true,
        daemon_pid: "14502",
        enabled: true,
        battery_cap: 68,
        battery_status: "Discharging (Bypass Active)",
        apps: [
          {
            pkg: "com.leohao.android.alistlite",
            installed: true,
            running: true,
            pids: "15210 15244",
            oom_score_adj: "-1000",
          },
        ],
      },
    });
  }
  if (cmd.includes("cat " + CONF_FILE)) {
    return Object.entries(currentConfig)
      .map(([k, v]) => `${k}="${v}"`)
      .join("\n");
  }
  if (cmd.includes("pm list packages -3")) {
    return "package:com.leohao.android.alistlite\npackage:com.termux\npackage:org.telegram.messenger\n";
  }
  return "[演示模式] 命令已执行: " + cmd;
}

function showToast(msg) {
  const el = document.getElementById("toast");
  el.textContent = msg;
  el.classList.remove("hidden");
  clearTimeout(window.__toastTimer);
  window.__toastTimer = setTimeout(() => el.classList.add("hidden"), 2800);
}

// --- Tab Navigation ---
document.querySelectorAll(".tab-btn").forEach((btn) => {
  btn.addEventListener("click", () => {
    document.querySelectorAll(".tab-btn").forEach((b) => b.classList.remove("active"));
    document.querySelectorAll(".tab-panel").forEach((p) => p.classList.remove("active"));
    btn.classList.add("active");
    document.getElementById(btn.dataset.tab).classList.add("active");
    if (btn.dataset.tab === "tab-logs") {
      refreshLogs();
    }
  });
});

// --- Load & Parse Config ---
async function loadConfigFromDevice() {
  const { stdout } = await execRoot(`cat ${CONF_FILE} 2>/dev/null || cat ${MODDIR}/config/config.conf 2>/dev/null`);
  stdout.split("\n").forEach((line) => {
    const m = line.trim().match(/^([A-Z0-9_]+)="?([^"]*)"?$/);
    if (m) {
      currentConfig[m[1]] = m[2];
    }
  });
  syncConfigToForm();
}

function syncConfigToForm() {
  document.getElementById("chk-router-autostart").checked = currentConfig.ROUTER_ENABLED === "1";
  document.getElementById("sel-vm-backend").value = currentConfig.VM_BACKEND || "auto";
  document.getElementById("sel-vm-cpus").value = currentConfig.VM_CPUS || "2";
  document.getElementById("sel-vm-mem").value = currentConfig.VM_MEM_MB || "256";
  document.getElementById("chk-hotspot-bridge").checked = currentConfig.HOTSPOT_BRIDGE_ENABLED === "1";
  document.getElementById("chk-host-redirect").checked = currentConfig.HOST_REDIRECT_ENABLED === "1";
  document.getElementById("chk-nas-mount").checked = currentConfig.NAS_MOUNT_ENABLED === "1";
  document.getElementById("input-nas-path").value = currentConfig.NAS_HOST_PATH || "/data/media/0/NAS";
  document.getElementById("chk-keepalive-enabled").checked = currentConfig.KEEPALIVE_ENABLED === "1";
  document.getElementById("chk-wakelock").checked = currentConfig.WAKELOCK_ENABLED === "1";
  document.getElementById("chk-bypass-charge").checked = currentConfig.BYPASS_CHARGE_ENABLED === "1";
}

async function saveConfigToDevice() {
  currentConfig.ROUTER_ENABLED = document.getElementById("chk-router-autostart").checked ? "1" : "0";
  currentConfig.VM_BACKEND = document.getElementById("sel-vm-backend").value;
  currentConfig.VM_CPUS = document.getElementById("sel-vm-cpus").value;
  currentConfig.VM_MEM_MB = document.getElementById("sel-vm-mem").value;
  currentConfig.HOTSPOT_BRIDGE_ENABLED = document.getElementById("chk-hotspot-bridge").checked ? "1" : "0";
  currentConfig.HOST_REDIRECT_ENABLED = document.getElementById("chk-host-redirect").checked ? "1" : "0";
  currentConfig.NAS_MOUNT_ENABLED = document.getElementById("chk-nas-mount").checked ? "1" : "0";
  currentConfig.NAS_HOST_PATH = document.getElementById("input-nas-path").value.trim() || "/data/media/0/NAS";
  currentConfig.KEEPALIVE_ENABLED = document.getElementById("chk-keepalive-enabled").checked ? "1" : "0";
  currentConfig.WAKELOCK_ENABLED = document.getElementById("chk-wakelock").checked ? "1" : "0";
  currentConfig.BYPASS_CHARGE_ENABLED = document.getElementById("chk-bypass-charge").checked ? "1" : "0";

  const content = Object.entries(currentConfig)
    .map(([k, v]) => `${k}="${v}"`)
    .join("\n");

  await execRoot(`mkdir -p /data/adb/x-route/config && cat <<'EOF' > ${CONF_FILE}\n${content}\nEOF`);
  showToast("配置已保存至 " + CONF_FILE);
}

// --- Refresh Live Status ---
async function refreshStatus() {
  const envBadge = document.getElementById("env-badge");
  envBadge.textContent = isKsuEnv() ? "SuKiSU Root 已连接" : "浏览器预览模式";

  const { stdout } = await execRoot(`sh ${MODDIR}/action.sh status-json`);
  let data;
  try {
    data = JSON.parse(stdout.trim());
  } catch (e) {
    return;
  }

  // 1. VM Card
  const vm = data.vm || {};
  const net = data.net || {};
  const ka = data.keepalive || {};

  const vmPill = document.getElementById("vm-status-pill");
  if (vm.running) {
    vmPill.textContent = "运行中";
    vmPill.className = "pill pill-running";
  } else {
    vmPill.textContent = "已停止";
    vmPill.className = "pill pill-stopped";
  }
  document.getElementById("vm-backend-val").textContent =
    vm.backend === "gunyah" ? "Gunyah (crosvm pVM)" : vm.backend === "qemu" ? "QEMU (ARM64)" : "未启动";
  document.getElementById("vm-resource-val").textContent = vm.running
    ? `PID ${vm.pid} · ${vm.cpus}核 / ${vm.mem_mb}MB`
    : "-";
  document.getElementById("gw-online-val").textContent = net.gw_online
    ? `在线 (${net.lan_gw || "192.168.100.1"})`
    : "离线 / 启动中";

  // 2. Network Card
  const netPill = document.getElementById("net-status-pill");
  if (net.gw_online) {
    netPill.textContent = "转发就绪";
    netPill.className = "pill pill-running";
  } else {
    netPill.textContent = "待命";
    netPill.className = "pill pill-warn";
  }
  document.getElementById("wan-if-val").textContent = net.cell_if || "未检测到蜂窝网卡";
  document.getElementById("ap-if-val").textContent = net.ap_if
    ? `${net.ap_if} -> br-lan (已接管)`
    : "热点未开启 (开启后0.5秒自动接管)";
  document.getElementById("redirect-state-val").textContent =
    net.host_redirect === "active"
      ? "已启用 (本机走 OpenWrt)"
      : net.host_redirect === "pending"
      ? "等待网关就绪..."
      : "未启用 (仅热点走 OpenWrt)";
  document.getElementById("lan-clients-val").textContent = `${net.lan_clients || 0} 台`;

  // 3. Keep-Alive Card
  const kaPill = document.getElementById("ka-status-pill");
  kaPill.textContent = ka.daemon_running ? `守护中 (PID ${ka.daemon_pid})` : "已停止";
  kaPill.className = ka.daemon_running ? "pill pill-running" : "pill pill-stopped";
  document.getElementById("battery-info-val").textContent = `${ka.battery_cap ?? "-"}% (${ka.battery_status || "-"})`;

  renderProtectedApps(ka.apps || []);
}

function renderProtectedApps(apps) {
  const container = document.getElementById("protected-apps-list");
  container.innerHTML = "";
  const configuredPkgs = (currentConfig.KEEPALIVE_PACKAGES || "")
    .split(/[\s,]+/)
    .map((s) => s.trim())
    .filter(Boolean);

  if (configuredPkgs.length === 0) {
    container.innerHTML = `<div class="hint">暂无保活应用，请在上方输入包名添加。</div>`;
    return;
  }

  configuredPkgs.forEach((pkg) => {
    const info = apps.find((a) => a.pkg === pkg) || { pkg, installed: true, running: false, pids: "", oom_score_adj: "N/A" };
    const row = document.createElement("div");
    row.className = "app-item";
    const statusText = !info.installed
      ? "未安装"
      : info.running
      ? `运行中 (PID: ${info.pids} · OOM: ${info.oom_score_adj})`
      : "未运行 (等待看门狗拉起)";

    row.innerHTML = `
      <div>
        <strong>${pkg}</strong>
        <div class="app-item-meta">${statusText}</div>
      </div>
      <button class="btn btn-danger btn-sm" data-remove-pkg="${pkg}">移除</button>
    `;
    container.appendChild(row);
  });

  container.querySelectorAll("[data-remove-pkg]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const target = btn.getAttribute("data-remove-pkg");
      const list = (currentConfig.KEEPALIVE_PACKAGES || "")
        .split(/[\s,]+/)
        .filter((p) => p && p !== target);
      currentConfig.KEEPALIVE_PACKAGES = list.join(" ");
      await saveConfigToDevice();
      await execRoot(`sh ${MODDIR}/scripts/keepalive_daemon.sh run-once`);
      refreshStatus();
    });
  });
}

// --- Button Handlers ---
document.getElementById("btn-start-router").addEventListener("click", async () => {
  showToast("正在启动 OpenWrt 软路由与网络编排...");
  await execRoot(`sh ${MODDIR}/action.sh start`);
  await refreshStatus();
  showToast("OpenWrt 软路由启动指令已下发！");
});

document.getElementById("btn-stop-router").addEventListener("click", async () => {
  showToast("正在安全停止软路由并还原网络...");
  await execRoot(`sh ${MODDIR}/action.sh stop`);
  await refreshStatus();
  showToast("软路由已停止，手机原生网络已恢复。");
});

document.getElementById("btn-sync-net").addEventListener("click", async () => {
  showToast("正在同步 WAN 与热点网桥...");
  await execRoot(`sh ${MODDIR}/scripts/net_orchestrator.sh reload`);
  await refreshStatus();
  showToast("网络与热点网桥同步完成！");
});

document.getElementById("btn-toggle-redirect").addEventListener("click", async () => {
  const next = currentConfig.HOST_REDIRECT_ENABLED === "1" ? "0" : "1";
  currentConfig.HOST_REDIRECT_ENABLED = next;
  syncConfigToForm();
  await saveConfigToDevice();
  if (next === "1") {
    await execRoot(`sh ${MODDIR}/scripts/net_orchestrator.sh enable-host-redirect`);
    showToast("已开启手机本机流量回流至 OpenWrt");
  } else {
    await execRoot(`sh ${MODDIR}/scripts/net_orchestrator.sh disable-host-redirect`);
    showToast("已关闭手机本机流量回流");
  }
  await refreshStatus();
});

document.getElementById("btn-add-pkg").addEventListener("click", async () => {
  const input = document.getElementById("input-new-pkg");
  const pkg = input.value.trim();
  if (!pkg) return;
  const set = new Set((currentConfig.KEEPALIVE_PACKAGES || "").split(/[\s,]+/).filter(Boolean));
  set.add(pkg);
  currentConfig.KEEPALIVE_PACKAGES = Array.from(set).join(" ");
  input.value = "";
  await saveConfigToDevice();
  await execRoot(`sh ${MODDIR}/scripts/keepalive_daemon.sh run-once`);
  await refreshStatus();
});

document.getElementById("btn-run-ka-now").addEventListener("click", async () => {
  showToast("正在执行四重内核级保活巡检...");
  await saveConfigToDevice();
  await execRoot(`sh ${MODDIR}/scripts/keepalive_daemon.sh run-once`);
  await refreshStatus();
  showToast("四重保活巡检已完成！");
});

document.getElementById("btn-scan-pkgs").addEventListener("click", async () => {
  const picker = document.getElementById("installed-picker");
  const listEl = document.getElementById("installed-pkg-list");
  picker.classList.remove("hidden");
  listEl.innerHTML = `<div class="hint">正在扫描已安装应用...</div>`;
  const { stdout } = await execRoot(`pm list packages -3 2>/dev/null | sed 's/package://g' | sort`);
  const pkgs = stdout.split("\n").map((s) => s.trim()).filter(Boolean);
  const currentSet = new Set((currentConfig.KEEPALIVE_PACKAGES || "").split(/[\s,]+/).filter(Boolean));

  listEl.innerHTML = "";
  pkgs.forEach((pkg) => {
    const row = document.createElement("div");
    row.className = "picker-row";
    const checked = currentSet.has(pkg) ? "checked" : "";
    row.innerHTML = `
      <span>${pkg}</span>
      <input type="checkbox" data-pick-pkg="${pkg}" ${checked} />
    `;
    listEl.appendChild(row);
  });

  listEl.querySelectorAll("[data-pick-pkg]").forEach((cb) => {
    cb.addEventListener("change", async () => {
      const p = cb.getAttribute("data-pick-pkg");
      const set = new Set((currentConfig.KEEPALIVE_PACKAGES || "").split(/[\s,]+/).filter(Boolean));
      if (cb.checked) set.add(p);
      else set.delete(p);
      currentConfig.KEEPALIVE_PACKAGES = Array.from(set).join(" ");
      await saveConfigToDevice();
      await refreshStatus();
    });
  });
});

document.getElementById("btn-close-picker").addEventListener("click", () => {
  document.getElementById("installed-picker").classList.add("hidden");
});

document.getElementById("btn-save-settings").addEventListener("click", async () => {
  await saveConfigToDevice();
  await execRoot(`sh ${MODDIR}/scripts/net_orchestrator.sh reload`);
  await execRoot(`sh ${MODDIR}/scripts/keepalive_daemon.sh run-once`);
  await refreshStatus();
});

async function refreshLogs() {
  const src = document.getElementById("sel-log-source").value;
  const viewer = document.getElementById("log-viewer");
  viewer.textContent = "正在读取...";
  let cmd = `tail -n 120 /data/adb/x-route/logs/x-route.log 2>/dev/null || echo '暂无日志'`;
  if (src === "vm-console") {
    cmd = `tail -n 120 /data/adb/x-route/logs/vm-console.log 2>/dev/null || echo '暂无虚拟机串口日志'`;
  } else if (src === "net-diag") {
    cmd = `echo '=== IP RULES ==='; ip rule show; echo '\n=== BR-LAN / TAP ==='; ip addr show dev br-lan 2>/dev/null; ip addr show dev tap-wan 2>/dev/null; echo '\n=== TABLE 100 ==='; ip route show table 100 2>/dev/null`;
  }
  const { stdout } = await execRoot(cmd);
  viewer.textContent = stdout || "无输出";
}

document.getElementById("btn-refresh-logs").addEventListener("click", refreshLogs);
document.getElementById("sel-log-source").addEventListener("change", refreshLogs);

// Initialize
(async function init() {
  await loadConfigFromDevice();
  await refreshStatus();
  setInterval(refreshStatus, 6000);
})();
