#!/bin/sh
# XRouter: WebUI 检查（源码 webui/，构建产物 module/webroot；需要 node，没有 node 时跳过）
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
W="$ROOT/webui"
command -v node >/dev/null 2>&1 || { echo "test-webui skip (no node)"; exit 0; }
fail=0
if grep -rnE '<select|window\.confirm|(^|[^[:alnum:]_.])confirm[[:space:]]*\(|alert\(|prompt\(' "$W/index.html" "$W/src"; then echo "FAIL: 使用了系统 select/confirm/alert"; fail=1; fi
if grep -rnE 'https?://(fonts\.|cdn|unpkg|esm\.sh)' "$W/index.html" "$W/src"; then echo "FAIL: 不得引用 CDN"; fail=1; fi
if grep -rn '<input type="file"' "$W/index.html"; then echo "FAIL: 应使用自定义文件浏览"; fail=1; fi
node --input-type=module - "$ROOT" <<'EOS' || fail=1
import fs from "fs";
const root = process.argv[2]; const W = root + "/webui";
let bad = 0; const fail = (m) => { console.log("FAIL: " + m); bad = 1; };
const eq = (a, b, m) => { if (JSON.stringify(a) !== JSON.stringify(b)) fail(`${m}: ${JSON.stringify(a)} != ${JSON.stringify(b)}`); };
const html = fs.readFileSync(W + "/index.html", "utf8");
const main = fs.readFileSync(W + "/src/main.js", "utf8");
const fb = fs.readFileSync(W + "/src/file-browser.js", "utf8");
const css = fs.readFileSync(W + "/src/style.css", "utf8");
const L = await import(W + "/src/logic.js");
const { ICONS } = await import(W + "/src/icons.js");
const cliMod = fs.readFileSync(root + "/rust/xr/src/cli/mod.rs", "utf8");
const configRs = fs.readFileSync(root + "/rust/xr/src/config.rs", "utf8");

// WebUI X 配置与依赖
const cfg = JSON.parse(fs.readFileSync(W + "/public/config.json", "utf8"));
eq([cfg.title, cfg.icon, cfg.windowResize, cfg.exitConfirm, cfg.backHandler, cfg.backInterceptor], ["XRouter", "icon.png", false, false, true, "native"], "config.json");
if (!fs.existsSync(W + "/public/icon.png")) fail("缺少 icon.png");
const pkg = JSON.parse(fs.readFileSync(W + "/package.json", "utf8"));
for (const d of ["@material/web", "kernelsu-alt"]) if (!pkg.dependencies[d]) fail(`缺少依赖 ${d}`);
if (!fs.existsSync(W + "/package-lock.json")) fail("缺少 package-lock.json");

// 彻底移除旧赛博朋克与电路板资产
for (const old of ["/src/pcb.css", "/src/pcb.js", "/src/frame.js", "/scripts/gen-pcb.py"]) {
  if (fs.existsSync(W + old)) fail(`不应保留旧赛博文件 ${old}`);
}
if (/Rajdhani|pcb-svg|chip-svg|scanline|cyberpunk/i.test(html + main + css)) fail("不得残留赛博朋克样式或组件");

// 元素 id、图标、xr 子命令一致
for (const src of [main, fb]) for (const m of src.matchAll(/\$\("([\w-]+)"\)/g)) if (!html.includes(`id="${m[1]}"`)) fail(`index.html 缺少 id=${m[1]}`);
for (const m of html.matchAll(/data-icon="(\w+)"/g)) if (!ICONS[m[1]]) fail(`图标未打包 ${m[1]}`);
for (const src of [main, fb]) for (const m of src.matchAll(/(?:setIcon\([^,]+,|iconEl\()\s*"(\w+)"/g)) if (!ICONS[m[1]]) fail(`图标未打包 ${m[1]}`);
const subs = new Set([...main.matchAll(/xr\("([\w-]+)"/g), ...fb.matchAll(/xr\("([\w-]+)"/g)].map((m) => m[1]));
for (const s of subs) if (!cliMod.includes(`"${s}" =>`) && !cliMod.includes(`cmd == "${s}"`)) fail(`xr 不支持子命令 ${s}`);
for (const s of ["status-json", "start-bg", "stop-bg", "traffic", "traffic-reset", "traffic-split", "set", "ipv6", "vpn-import", "vpn-enable", "vpn-disable", "vpn-delete", "ls", "log-export", "diag-export", "uninstall-now", "expand", "reset-disk", "ack-trip", "luci", "project"]) {
  if (!subs.has(s)) fail(`WebUI 未使用 ${s}`);
}

// Material Design 3 导航栏与三页信息架构（状态 · 网络 · 系统）
const navTabs = [...html.matchAll(/<md-navigation-tab[^>]*label="([^"]+)"/g)].map((m) => m[1]);
eq(navTabs, ["状态", "网络", "系统"], "M3 底部导航栏");
const pages = [...html.matchAll(/<section class="page(?:\s+on)?" id="([^"]+)">/g)].map((m) => m[1]);
eq(pages, ["page-home", "page-net", "page-system"], "三页容器");
if (!/const PAGE_MAX = 2;/.test(main)) fail("PAGE_MAX 应为 2（对应三页）");
if (html.includes("<md-tabs") || html.includes("<md-primary-tab")) fail("不应再使用旧顶部标签栏");
if (html.indexOf('class="appbar"') > html.indexOf('id="pager"') || html.indexOf('id="pager"') > html.indexOf('id="nav"')) fail("布局顺序应为顶栏 -> 页面容器 -> 底部导航栏");

// M3 色彩令牌、自动明暗主题与安全区适配
if (!html.includes("viewport-fit=cover")) fail("缺少 viewport-fit=cover");
if (!css.includes("safe-area-inset-top") || !css.includes("safe-area-inset-bottom")) fail("缺少 safe-area 变量");
for (const tok of ["--md-sys-color-primary", "--md-sys-color-primary-container", "--md-sys-color-secondary-container", "--md-sys-color-tertiary-container", "--md-sys-color-surface-container-low", "--md-sys-color-error-container"]) {
  if (!css.includes(tok)) fail(`缺少 M3 色彩令牌 ${tok}`);
}
if (!css.includes("@media (prefers-color-scheme: dark)") || !css.includes(':root[data-theme="dark"]') || !html.includes('id="btn-theme"')) fail("应支持自动明暗主题与手动切换");
if (css.includes("breathe") || /animation:\s*breathe/.test(css) || css.includes("@keyframes breathe")) fail("不得保留呼吸光动画");
if (css.includes("backdrop-filter") || /filter:\s*blur/.test(css)) fail("不得使用大面积模糊");

// M3 主状态卡、精简话术、高密度只读指标磁贴与页面分区归属
if (!html.includes('id="hero-card"') || !html.includes('class="wave-svg"') || !html.includes('id="boot-steps"')) fail("状态页应有 M3 主状态卡与波浪进度");
for (const t of ["开机自启", "运行时长", "上游网络", "热点分流", "组网状态", "打开 LuCI", "导入配置", "连接状态", "路由网段", "运行模式", "管理访问", "IPv6",
  "异地组网", "常规与通知", "流量监测", "公网流量", "内网流量", "手机流量", "热点流量",
  "存储共享", "目录", "资源", "CPU", "内存", "系统盘", "扩容", "重置", "选择此文件夹", "日志导出", "DHCP", "网关地址", "地址/掩码", "整机复位后自启已关闭",
  "关于", "项目地址", "https://github.com/jjdufu/x-route", "作者", "jjdufu", "卸载 XRouter", "立即卸载", "通知显示", "累计流量", "清零"]) {
  if (!html.includes(t)) fail(`缺少文案 ${t}`);
}
for (const t of ["谁可以访问", "互联", "上传并启用", "已接入的网段", "共享手机存储", "仅本机", "热点设备", "当前局域网", "控制台", "控制通道", "虚拟机管理器", "SSH", "虚拟机资源", "热点 DHCP", "热点网段", "网关地址/前缀", "op13",
  "启动服务", "正在启动", "正在停止", "重新启动", "流量与通知"]) {
  if (html.includes(t) || main.includes(t)) fail(`旧文案残留 ${t}`);
}
if (!html.includes('<span id="power-btn-text">启动</span>')) fail("主操作按钮初始文案应精简为「启动」");
if (/<button[^>]*class="metric-tile"|tile-arrow/.test(html + css)) fail("状态页指标磁贴应为只读展示，无无效跳转箭头");
if (!/flex-wrap:\s*nowrap/.test(css.slice(css.indexOf(".hero-actions {"), css.indexOf("}", css.indexOf(".hero-actions {"))))) fail("主状态卡双按钮在窄屏不应换行挤占");

// 校验模块在三个页面的归属（流量监测在状态页、异地组网在网络页、开机自启与通知显示在系统页）
const homeHtml = html.slice(html.indexOf('id="page-home"'), html.indexOf('id="page-net"'));
const netHtml = html.slice(html.indexOf('id="page-net"'), html.indexOf('id="page-system"'));
const sysHtml = html.slice(html.indexOf('id="page-system"'), html.indexOf("</main>"));
if (!homeHtml.includes('id="v-traffic"') || !homeHtml.includes('id="v-tr-wan"') || !homeHtml.includes('id="tr-ph-total"') || html.includes('id="dlg-traffic"')) fail("详细流量监测应内嵌在状态页");
if (!netHtml.includes('id="vpn-list"') || !netHtml.includes("异地组网")) fail("异地组网应合并在网络页");
if (!sysHtml.includes('id="row-autostart"') || !sysHtml.includes('id="seg-notify"')) fail("开机自启与通知显示应位于系统页");

// 页面切换：150-200ms 干脆横移淡入（WAAPI），无滚动吸附、无 smooth 滚动
if (/scroll-snap|scroll-behavior:\s*smooth/.test(css)) fail("页面切换不得使用滚动吸附动画");
if (/behavior:\s*["']smooth|behavior:\s*smooth/.test(main)) fail("页面切换不得使用 smooth 滚动");
{ const m = main.match(/const PAGE_MS = (\d+);/); if (!m || +m[1] < 150 || +m[1] > 200) fail("切页动画应 150-200ms"); }
if (!/pages\[i\]\.animate\(/.test(main) || !/duration: PAGE_MS, easing: "cubic-bezier\(\.2,\.8,\.2,1\)"/.test(main)) fail("切页应 WAAPI ease-out 横移淡入");
if (!/\.page:not\(\.on\)/.test(css) || !html.includes('class="page on" id="page-home"')) fail("页面应按 .on 直接显隐");
if (/\.page[^{]*\{[^}]*(transition|animation(?!-play-state))/.test(css)) fail("页面容器本身不得带常驻过渡动画");

const visible = html.replace(/<script[\s\S]*?<\/script>/g, "").replace(/<[^>]+>/g, " ");
if (/XR_|VM_|AUTOSTART|hotspot|bypass|global|ovpn|SWIOTLB/.test(visible)) fail("页面可见文案含内部变量名或取值");

eq(L.MODE_OPTS.map((o) => o.label), ["全局接管", "旁路网关"], "运行模式");
eq(L.ACCESS_OPTS.map((o) => o.label), ["本机", "内网", "所有"], "管理访问");
eq(L.SHARE_OPTS.map((o) => o.label), ["只读", "读写"], "权限");
eq(L.NOTIFY_OPTS.map((o) => o.label), ["总流量", "公网", "内网"], "通知显示");
const notifyVals = [...configRs.match(/"XR_NOTIFY_TRAFFIC" => matches!\(val, ([^)]+)\)/)[1].matchAll(/"(\w+)"/g)].map((m) => m[1]);
eq(L.NOTIFY_OPTS.map((o) => o.value), notifyVals, "通知显示取值与 valid_kv 一致");
if (!/XR_NOTIFY_TRAFFIC="total"/.test(fs.readFileSync(root + "/module/config/config.conf", "utf8"))) fail("通知显示默认应为总流量");
eq([L.fmtBytes(0), L.fmtBytes(1023), L.fmtBytes(1536), L.fmtBytes(1288490189), L.fmtBytes(9234179686), L.fmtBytes(5497558138880)],
  ["0 B", "1023 B", "1.5 KB", "1.2 GB", "8.6 GB", "5.0 TB"], "fmtBytes");
const tj = { wan: { up: 1024, down: 2048 }, lan: { up: 0, down: 1024 }, total: { up: 1024, down: 3072 } };
eq([L.trafficLine(tj, "total"), L.trafficLine(tj, "wan"), L.trafficLine(tj, "lan"), L.trafficLine(null, "total")],
  ["↑1.0 KB ↓3.0 KB", "↑1.0 KB ↓2.0 KB", "↑0 B ↓1.0 KB", "—"], "trafficLine");
eq(L.fmtSince(0), "累计统计", "fmtSince 缺省");
if (!/^自 \d\d-\d\d \d\d:\d\d$/.test(L.fmtSince(1731400000))) fail("fmtSince 时间格式");

// 流量监测在「状态」页（page === 0）按需轮询与切回刷新
if (!/page === 0\) loadTraffic\(\)/.test(main) || !/if \(i === 0\) requestAnimationFrame\(\(\) => setTimeout\(loadTraffic, 0\)\)/.test(main)) fail("流量监测应在状态页实时更新");

// 日志统一：系统页「日志」分组按调试模式切换「日志导出 / 诊断导出」
if ("LOG_KINDS" in L || html.includes('id="page-logs"') || html.includes("log-kinds") || /xr\("log",|"log-clear"/.test(main)) fail("不应再有日志页与日志分类");
if (!html.includes('<div class="group-title">日志</div>') || html.includes('<div class="group-title">调试</div>')) fail("分组应为「日志」");
if (!/"诊断导出" : "日志导出"/.test(main) || !/diag \? await xr\("diag-export"\) : await xr\("log-export"\)/.test(main)) fail("导出按钮应随调试模式切换");

const exposeVals = [...configRs.match(/"XR_EXPOSE" => matches!\(val, ([^)]+)\)/)[1].matchAll(/"(\w+)"/g)].map((m) => m[1]);
eq(L.ACCESS_OPTS.map((o) => o.value), exposeVals, "管理访问取值与 valid_kv 一致");
eq(L.MODE_OPTS.map((o) => o.value), ["global", "bypass"], "运行模式取值");
if (!/XR_EXPOSE="lan"/.test(fs.readFileSync(root + "/module/config/config.conf", "utf8"))) fail("管理访问默认应为内网");
for (const k of L.SET_KEYS) if (!new RegExp(`\\b${k}\\b`).test(configRs)) fail(`SET_KEYS 含未知键 ${k}`);

// 确认弹窗范围（含删除组网配置）
const confirms = [...main.matchAll(/confirmDlg\([`"]([^`"]+)/g)].map((m) => m[1]);
eq(confirms.sort(), ["切换为${label}", "停止服务", "删除配置 ${name}", "${label}改为 ${fmt(v)}", "清零流量统计", "重置系统盘", "立即卸载 XRouter"].sort(), "确认弹窗范围");
if (!main.includes('dialogResult($("dlg-expand"))')) fail("扩容需确认");

// 纯函数测试
eq(L.shq("a'b"), "'a'\\''b'", "shq");
eq(L.xrCmd("set", "VM_SHARE_PATH", "$(reboot)"), "sh /data/adb/modules/xrouter/scripts/xr set 'VM_SHARE_PATH' '$(reboot)'", "xrCmd 引号");
let threw = false; try { L.xrCmd("set;reboot"); } catch { threw = true; } if (!threw) fail("xrCmd 未拒绝非法子命令");
eq(L.lastJson('x\n{"ok":true}'), { ok: true }, "lastJson");
const st = (vm, net = {}, config = {}) => ({ vm, net: { gw: "block", ...net }, config: { XR_MODE: "global", ...config } });
eq(L.vmView(st({ running: false, tripped: null })).phase, "stopped", "停止");
eq(L.vmView(st({ running: false, tripped: null }), "start"), { phase: "starting", chip: "启动中", text: "启动中", step: 0 }, "点击启动");
eq(L.vmView(st({ running: true, state: "booting" })).text, "启动中", "booting");
eq(L.vmView(st({ running: true, state: "ready" }, { gw: "off" })).text, "网络就绪", "等待接管");
eq(L.vmView(st({ running: true, state: "ready" }, { gw: "off" }, { XR_MODE: "bypass" })).phase, "running", "旁路就绪即运行");
eq(L.vmView(st({ running: true, state: "stable" })), { phase: "running", chip: "运行中", text: "运行中", step: 2 }, "运行中");
eq(L.vmView(st({ running: true, state: "stable" }), "stop"), { phase: "stopping", chip: "停止中", text: "停止中" }, "停止中");
eq(L.vmView(st({ running: false, error: "180 秒内未就绪" })), { phase: "failed", chip: "异常", text: "180 秒内未就绪" }, "失败原因");
eq(L.vmView(st({ running: false, tripped: "2026-11-10" })).phase, "failed", "熔断");
eq(L.fmtUptime(65), "1 分 5 秒", "uptime"); eq(L.fmtUptime(90061), "1 天 1 小时", "uptime 天");
eq([L.upKind("wlan0"), L.upKind("rmnet_data0"), L.upKind("")], ["Wi-Fi", "蜂窝", "无"], "上游");
eq(L.displayPath("/data/media/0/Download/x"), "内部存储/Download/x", "显示路径");
eq(L.toMediaPath("/sdcard/a"), "/storage/emulated/0/a", "sdcard");
eq(L.crumbs("/data/media/0/A/B").map((c) => c.label), ["内部存储", "A", "B"], "面包屑");
eq(L.crumbs("/storage/emulated/0/A").map((c) => c.path), ["/storage/emulated/0", "/storage/emulated/0/A"], "面包屑路径");
eq(L.parseLs("d/A\nf/x.ovpn\nd/.hidden\njunk"), [{ dir: true, name: "A" }, { dir: false, name: "x.ovpn" }], "parseLs");
eq(L.expandSizes(1024), [1536, 2048, 3072, 5120, 9216], "扩容选项"); eq(L.expandSizes(16384), [], "扩容上限");

// 性能与交互不变量
if (subs.has("start") || subs.has("stop")) fail("WebUI 不应同步调用 xr start/stop");
if (/await\s+xr\(\s*kind/.test(main)) fail("按钮不得等待后台启停");
if (!/canSpawn\(\) \? await runSpawn\(cmd\) : await exec\(cmd\)/.test(main)) fail("xr() 应优先用 spawn");
if (!/if \(refreshing\) \{ refreshAgain = true; return refreshing; \}/.test(main)) fail("refresh 应单飞");
{ const sk = main.slice(main.indexOf("async function setKey"), main.indexOf("// ---- 渲染 ----"));
  if (sk.indexOf("render(status)") < 0 || sk.indexOf("render(status)") > sk.indexOf('await xr("set"') || /await refresh\(\)/.test(sk)) fail("setKey 应乐观更新、不等 refresh"); }
if (!/for \(const \[k, e\] of edits\) if \(e\.doneAt !== null && t0 > e\.doneAt\) edits\.delete\(k\);/.test(main)) fail("保存前发出的刷新不能把开关弹回去");
if (!/v\.phase === "starting"/.test(main) || !/fast \? 1500 : 5000/.test(main)) fail("启动中应加快状态刷新");
if (!/const txt = \(el, v\) =>/.test(main) || /\$\("v-uptime"\)\.textContent =/.test(main)) fail("状态刷新应只在变化时写 DOM");
if (!/class="shell nostate/.test(html) || !/localStorage/.test(main) || !/\.nostate/.test(css)) fail("重新打开不应闪「已停止」");
if (!/prefers-reduced-motion/.test(css) || !/prefersReduce/.test(main)) fail("应尊重减少动效");
if (!/\.hid \.wave-active/.test(css) || !/classList\.toggle\("hid", document\.hidden\)/.test(main)) fail("不可见时应暂停波浪进度动画");

const blk = (sel) => css.slice(css.indexOf(sel + " {"), css.indexOf("}", css.indexOf(sel + " {")));
const bt = L.parseBootTail("R1\x1f120\x1f2026-11-11 09:00:00 [INFO] [vm] 已启动 pid=1\n\x1f30\x1f[    0.000000] Booting Linux\n[ 0.1] half");
eq([bt.rc, bt.mod.size, bt.mod.text ? 1 : 0, bt.con.size], [1, 120, 1, 30], "parseBootTail");
eq(L.parseBootTail("R-\x1f0\x1f\x1f0\x1f").rc, null, "parseBootTail 无退出码");
eq(L.parseBootTail("garbage"), null, "parseBootTail 非法");
for (const v of ["", "10.233.0.1/24", "192.168.77.254/24", "172.20.0.1/16", "10.9.9.1/30"]) eq(L.hsSubnetCheck(v).ok, true, `hsSubnetCheck ${v}`);
for (const v of ["10.233.0.0/24", "10.233.0.255/24", "10.233.0.1/15", "10.233.0.1/31", "8.8.8.1/24", "100.64.0.1/24", "10.233.0.1", "10.233.0.300/24", "010.233.0.1/24", "10.008.0.1/24", "x"]) eq(L.hsSubnetCheck(v).ok, false, `hsSubnetCheck ${v}`);
const bb = L.newBootBuf();
eq(L.feedBoot(bb, "mod", bt.mod.text), true, "feedBoot 有新行");
L.feedBoot(bb, "con", bt.con.text);
eq(bb.lines, ["[vm] 已启动 pid=1", "[    0.000000] Booting Linux"], "feedBoot 去时间前缀、保留半行");
eq(L.bootStep(10, 10), 10, "bootStep 无积压");

// 立即卸载：危险样式 + 确认；先切换静态「已卸载」页停止一切轮询，再请求后台删除
if (!/id="btn-uninstall" class="danger"/.test(html) || !/id="gone" class="gone" hidden/.test(html)) fail("系统页应有立即卸载按钮与静态已卸载页");
{ const u = main.slice(main.indexOf("async function uninstallNow"), main.indexOf("// ---- 累计流量"));
  const iC = u.indexOf("confirmDlg("), iR = u.indexOf('.shell").remove()'), iX = u.indexOf('await xr("uninstall-now", "confirm")');
  if (iC < 0 || iR < 0 || iX < 0 || !(iC < iR && iR < iX) || !/Download/.test(u) || !/, true\)\) return;/.test(u)) fail("卸载应先确认、再换静态页、最后请求删除");
  if (!/gone = true;/.test(u) || !/if \(gone\) return Promise\.resolve\(\);/.test(main) || !/clearTimeout\(timer\);\n  if \(gone\) return;/.test(main)) fail("卸载后应停止一切轮询"); }
if (/animation|transition/.test(blk(".gone"))) fail("已卸载页应是静态的");

// 异地组网（合并在网络页）：导入即展示多配置卡片，每张卡片直接独立启动 / 停止 / 删除，无下拉框选择
if (!html.includes('id="tpl-vpn"') || !html.includes('id="vpn-list"') || !html.includes('id="vpn-empty"') || !/function renderVpn/.test(main)) fail("异地组网应支持空状态与多配置列表渲染");
if (!html.includes('class="btn-vpn-start"') || !html.includes('class="btn-vpn-stop"') || !html.includes('class="danger btn-vpn-del"')) fail("每张组网卡片应包含启动 / 停止 / 删除按钮");
if (!/xr\("vpn-import", path, "0"\)/.test(main)) fail("WebUI 导入配置应默认传 0 以便在列表中按需启动");
if (!/function actVpn/.test(main) || !/function delVpn/.test(main)) fail("异地组网应支持按配置启停与删除");

eq(L.vpnProfiles({ configured: false, profiles: [], tunnels: [] }), [], "未导入配置时为空列表");
const vp = L.vpnProfiles({
  configured: true,
  routes_active: ["10.8.0.0/24", "192.168.31.0/24", "192.168.50.0/24"],
  profiles: [
    { id: "home", name: "home-nas", enabled: true, status: "up", dev: "tun0", addr: "10.8.0.5/24", routes: ["10.8.0.0/24", "192.168.31.0/24"] },
    { id: "office", name: "公司内网", enabled: false, status: "down", dev: "", addr: "", routes: [] },
  ],
  tunnels: [
    { dev: "tun0", name: "home", status: "up", addr: "10.8.0.5/24", routes: ["10.8.0.0/24", "192.168.31.0/24"] },
    { dev: "tun2", name: "luci_ext", status: "up", addr: "10.10.0.2/24", routes: ["192.168.50.0/24"] },
  ],
});
eq(vp.map((p) => [p.id, p.name, p.status, p.addr, p.routes, p.managed]), [
  ["home", "home-nas", "up", "10.8.0.5", ["10.8.0.0/24", "192.168.31.0/24"], true],
  ["office", "公司内网", "down", "", [], true],
  ["", "luci_ext", "up", "10.10.0.2", ["192.168.50.0/24"], false],
], "vpnProfiles 多配置与外部隧道");

const vt = L.vpnTunnels({ status: "up", addr: "10.8.0.5/24", routes_active: ["10.8.0.0/24", "192.168.50.0/24", "192.168.31.0/24"],
  tunnels: [{ dev: "tun0", name: "home", status: "up", addr: "10.8.0.5/24", routes: ["10.8.0.0/24", "192.168.31.0/24"] }, { dev: "tun1", name: "office", status: "up", addr: "10.9.0.6/24", routes: ["192.168.50.0/24"] }] });
eq(vt.map((t) => [t.name, t.addr, t.routes]), [["home", "10.8.0.5", ["10.8.0.0/24", "192.168.31.0/24"]], ["office", "10.9.0.6", ["192.168.50.0/24"]]], "vpnTunnels 多隧道");

const sl = L.splitLines({ ok: true, phone: { wan: { up: 1024, down: 2048 }, lan: { up: 0, down: 0 }, total: { up: 1024, down: 2048 } }, hotspot: { wan: { up: 0, down: 0 }, lan: { up: 0, down: 1024 }, total: { up: 0, down: 1024 } } });
eq([sl.phone.total, sl.phone.wan, sl.hotspot.lan], ["↑1.0 KB ↓2.0 KB", "↑1.0 KB ↓2.0 KB", "↑0 B ↓1.0 KB"], "splitLines");
eq(L.splitLines({ ok: false }), null, "splitLines 失败");
for (const id of ["v-tr-wan", "v-tr-lan", "tr-ph-total", "tr-ph-wan", "tr-ph-lan", "tr-hs-total", "tr-hs-wan", "tr-hs-lan"]) if (!homeHtml.includes(`id="${id}"`)) fail(`状态页流量监测缺少 ${id}`);
if (!/function diskLocked/.test(main) || !main.includes("停止后可修改")) fail("运行中系统盘应置灰并提示「停止后可修改」");

process.exit(bad);
EOS
# 构建产物（有依赖时构建到临时目录检查）
if [ -d "$W/node_modules" ]; then
  T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT
  if (cd "$W" && npx vite build --outDir "$T/out" --emptyOutDir --logLevel error) >/dev/null; then
    for f in index.html config.json icon.png; do [ -f "$T/out/$f" ] || { echo "FAIL: 产物缺少 $f"; fail=1; }; done
    ls "$T/out/assets/"*.woff2 >/dev/null 2>&1 || { echo "FAIL: 产物缺少图标字体"; fail=1; }
    if grep -rqE 'fonts\.googleapis|fonts\.gstatic' "$T/out"; then echo "FAIL: 产物引用在线字体"; fail=1; fi
  else
    echo "FAIL: vite build"; fail=1
  fi
fi
if [ "$fail" = 0 ]; then echo "test-webui OK"; else echo "test-webui FAILED"; exit 1; fi
