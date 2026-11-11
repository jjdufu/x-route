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
const L = await import(W + "/src/logic.js");
const { ICONS } = await import(W + "/src/icons.js");
const xrsh = fs.readFileSync(root + "/module/scripts/xr", "utf8");
const common = fs.readFileSync(root + "/module/scripts/common.sh", "utf8");

// WebUI X 配置
const cfg = JSON.parse(fs.readFileSync(W + "/public/config.json", "utf8"));
eq([cfg.title, cfg.icon, cfg.windowResize, cfg.exitConfirm, cfg.backHandler, cfg.backInterceptor], ["XRouter", "icon.png", false, false, true, "native"], "config.json");
if (!fs.existsSync(W + "/public/icon.png")) fail("缺少 icon.png");
const pkg = JSON.parse(fs.readFileSync(W + "/package.json", "utf8"));
for (const d of ["@material/web", "kernelsu-alt"]) if (!pkg.dependencies[d]) fail(`缺少依赖 ${d}`);
if (!fs.existsSync(W + "/package-lock.json")) fail("缺少 package-lock.json");

// 元素 id、图标、xr 子命令一致
for (const src of [main, fb]) for (const m of src.matchAll(/\$\("([\w-]+)"\)/g)) if (!html.includes(`id="${m[1]}"`)) fail(`index.html 缺少 id=${m[1]}`);
for (const m of html.matchAll(/data-icon="(\w+)"/g)) if (!ICONS[m[1]]) fail(`图标未打包 ${m[1]}`);
for (const src of [main, fb]) for (const m of src.matchAll(/(?:setIcon\([^,]+,|iconEl\()\s*"(\w+)"/g)) if (!ICONS[m[1]]) fail(`图标未打包 ${m[1]}`);
const subs = new Set([...main.matchAll(/xr\("([\w-]+)"/g), ...fb.matchAll(/xr\("([\w-]+)"/g)].map((m) => m[1]));
for (const s of subs) if (!new RegExp(`^  ${s}\\)`, "m").test(xrsh) && !xrsh.includes(`if [ "\${1:-}" = ${s} ]`)) fail(`xr 不支持子命令 ${s}`);
for (const s of ["status-json", "start-bg", "stop-bg", "boot-tail", "traffic", "traffic-reset", "set", "ipv6", "vpn-import", "ls", "log-export", "diag-export", "expand", "reset-disk", "ack-trip", "luci"]) if (!subs.has(s)) fail(`WebUI 未使用 ${s}`);

// 布局与文案
const tabs = [...html.matchAll(/<md-primary-tab[^>]*>.*?<\/md-icon>([^<]+)<\/md-primary-tab>/g)].map((m) => m[1].trim());
eq(tabs, ["主页", "组网", "设置"], "顶部 Tab");
if (/bottom|tabbar|nav class/.test(html)) fail("不应有底部导航");
if (html.indexOf('class="appbar"') > html.indexOf('id="pager"')) fail("标题栏必须在内容区之外");
const css = fs.readFileSync(W + "/src/style.css", "utf8");
if (!html.includes("viewport-fit=cover")) fail("缺少 viewport-fit=cover");
if (!css.includes("safe-area-inset-top") || !css.includes("safe-area-inset-bottom")) fail("缺少 safe-area 变量");
if (css.includes("breathe") || /animation:\s*breathe/.test(css) || css.includes("@keyframes breathe")) fail("不得保留呼吸光动画");
if (css.includes("backdrop-filter") || /filter:\s*blur/.test(css)) fail("不得使用大面积模糊");
if (html.includes("概览") || main.includes("概览") || css.includes("概览")) fail("页签名应为「主页」");
for (const t of ["开机自启", "运行时长", "上游网络", "打开 LuCI", "导入配置", "连接状态", "路由网段", "运行模式", "管理访问", "IPv6",
  "存储共享", "目录", "虚拟机资源", "CPU", "内存", "系统盘", "扩容", "重置", "选择此文件夹", "日志导出", "热点 DHCP", "整机复位后自启已关闭"]) {
  if (!html.includes(t)) fail(`缺少文案 ${t}`);
}
for (const t of ["谁可以访问", "互联", "上传并启用", "已接入的网段", "共享手机存储", "仅本机", "热点设备", "当前局域网", "控制台", "控制通道", "虚拟机管理器", "SSH"]) {
  if (html.includes(t) || main.includes(t)) fail(`旧文案残留 ${t}`);
}
// 页面切换必须瞬时：无横向滚动吸附、无 smooth 滚动、关闭 Tab 指示条动画
if (/scroll-snap|scroll-behavior:\s*smooth/.test(css)) fail("页面切换不得使用滚动吸附动画");
if (/behavior:\s*["']smooth|behavior:\s*smooth/.test(main)) fail("页面切换不得使用 smooth 滚动");
if (!/Tab\.prototype\[ANIMATE_INDICATOR\]\s*=/.test(main)) fail("未关闭 Tab 指示条动画");
if (!/\.page:not\(\.on\)/.test(css) || !html.includes('class="page on" id="page-home"')) fail("页面应按 .on 直接显隐");
if (/\.page[^{]*\{[^}]*(transition|animation(?!-play-state))/.test(css)) fail("页面不得带过渡动画");
const visible = html.replace(/<script[\s\S]*?<\/script>/g, "").replace(/<[^>]+>/g, " ");
if (/XR_|VM_|AUTOSTART|hotspot|bypass|global|ovpn|SWIOTLB/.test(visible)) fail("页面可见文案含内部变量名或取值");
eq(L.MODE_OPTS.map((o) => o.label), ["全局接管", "旁路网关"], "运行模式");
eq(L.ACCESS_OPTS.map((o) => o.label), ["本机", "内网", "所有"], "管理访问");
eq(L.SHARE_OPTS.map((o) => o.label), ["只读", "读写"], "权限");
eq(L.NOTIFY_OPTS.map((o) => o.label), ["总流量", "公网", "内网"], "通知显示");
const notifyVals = common.match(/XR_NOTIFY_TRAFFIC\)\n\s*case "\$2" in ([\w|]+)\)/)[1].split("|");
eq(L.NOTIFY_OPTS.map((o) => o.value), notifyVals, "通知显示取值与 valid_kv 一致");
if (!/XR_NOTIFY_TRAFFIC="total"/.test(fs.readFileSync(root + "/module/config/config.conf", "utf8"))) fail("通知显示默认应为总流量");
for (const t of ["通知显示", "累计流量", "清零"]) if (!html.includes(t)) fail(`缺少文案 ${t}`);
eq([L.fmtBytes(0), L.fmtBytes(1023), L.fmtBytes(1536), L.fmtBytes(1288490189), L.fmtBytes(9234179686), L.fmtBytes(5497558138880)],
  ["0 B", "1023 B", "1.5 KB", "1.2 GB", "8.6 GB", "5.0 TB"], "fmtBytes");
const tj = { wan: { up: 1024, down: 2048 }, lan: { up: 0, down: 1024 }, total: { up: 1024, down: 3072 } };
eq([L.trafficLine(tj, "total"), L.trafficLine(tj, "wan"), L.trafficLine(tj, "lan"), L.trafficLine(null, "total")],
  ["↑1.0 KB ↓3.0 KB", "↑1.0 KB ↓2.0 KB", "↑0 B ↓1.0 KB", "—"], "trafficLine");
// 累计流量只在设置页读取，不在主页轮询
if (!/page === 2\) loadTraffic\(\)/.test(main)) fail("累计流量应只在设置页读取");
// 日志统一：没有独立日志页与分类，设置里的「日志」分组按调试模式切换「日志导出 / 诊断导出」
if ("LOG_KINDS" in L || html.includes('id="page-logs"') || html.includes("log-kinds") || /xr\("log",|"log-clear"/.test(main)) fail("不应再有日志页与日志分类");
if ((html.match(/<md-primary-tab /g) || []).length !== 3) fail("应只剩 主页 / 组网 / 设置 三页");
if (!html.includes('<div class="group-title">日志</div>') || html.includes('<div class="group-title">调试</div>')) fail("设置分组应改名为「日志」");
if (!/"诊断导出" : "日志导出"/.test(main) || !/diag \? await xr\("diag-export"\) : await xr\("log-export"\)/.test(main)) fail("导出按钮应随调试模式切换");
const exposeVals = common.match(/XR_EXPOSE\)\n\s*case "\$2" in ([\w|]+)\)/)[1].split("|");
eq(L.ACCESS_OPTS.map((o) => o.value), exposeVals, "管理访问取值与 valid_kv 一致");
eq(L.MODE_OPTS.map((o) => o.value), ["global", "bypass"], "运行模式取值");
if (!/XR_EXPOSE="lan"/.test(fs.readFileSync(root + "/module/config/config.conf", "utf8"))) fail("管理访问默认应为内网");
for (const k of L.SET_KEYS) if (!new RegExp(`\\b${k}\\b`).test(common)) fail(`SET_KEYS 含未知键 ${k}`);

// 确认弹窗只用于：运行模式、停止、重置 / 扩容系统盘、清空日志
const confirms = [...main.matchAll(/confirmDlg\([`"]([^`"]+)/g)].map((m) => m[1]);
eq(confirms.sort(), ["切换为${label}", "停止虚拟机", "清零流量统计", "重置系统盘"].sort(), "确认弹窗范围");
if (!main.includes('dialogResult($("dlg-expand"))')) fail("扩容需确认");

// 纯函数
eq(L.shq("a'b"), "'a'\\''b'", "shq");
eq(L.xrCmd("set", "VM_SHARE_PATH", "$(reboot)"), "sh /data/adb/modules/xrouter/scripts/xr set 'VM_SHARE_PATH' '$(reboot)'", "xrCmd 引号");
let threw = false; try { L.xrCmd("set;reboot"); } catch { threw = true; } if (!threw) fail("xrCmd 未拒绝非法子命令");
eq(L.lastJson('x\n{"ok":true}'), { ok: true }, "lastJson");
const st = (vm, net = {}, config = {}) => ({ vm, net: { gw: "block", ...net }, config: { XR_MODE: "global", ...config } });
eq(L.vmView(st({ running: false, tripped: null })).phase, "stopped", "停止");
eq(L.vmView(st({ running: false, tripped: null }), "start"), { phase: "starting", chip: "已停止", text: "启动虚拟机", step: 0 }, "点击启动");
eq(L.vmView(st({ running: true, state: "booting" })).text, "启动虚拟机", "booting");
eq(L.vmView(st({ running: true, state: "ready" }, { gw: "off" })).text, "网络就绪", "等待接管");
eq(L.vmView(st({ running: true, state: "ready" }, { gw: "off" }, { XR_MODE: "bypass" })).phase, "running", "旁路就绪即运行");
eq(L.vmView(st({ running: true, state: "stable" })), { phase: "running", chip: "运行中", text: "运行中", step: 2 }, "运行中");
eq(L.vmView(st({ running: true, state: "stable" }), "stop").phase, "stopping", "停止中");
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
// 启停与按钮解耦：只投递后台命令，不等待 xr start/stop
if (subs.has("start") || subs.has("stop")) fail("WebUI 不应同步调用 xr start/stop");
if (/await\s+xr\(\s*kind/.test(main)) fail("按钮不得等待后台启停");
// 开机日志：真实增量日志、轮询节流、固定行池、只用 transform 平滑上移、无模糊/发光动画
if (!html.includes('id="boot-log"') || html.includes('id="power-log"')) fail("开机日志应在舞台里而不是按钮里");
if (html.indexOf('id="boot"') > html.indexOf('id="row-autostart"')) fail("开机日志应在「开机自启」上方");
{ const m = main.match(/const BOOT_POLL = (\d+);/); if (!m || +m[1] < 150 || +m[1] > 250 || !/setTimeout\(\(\) => bootTick\(b\), BOOT_POLL\)/.test(main)) fail("开机日志应 150-250ms 轮询"); }
// 点击不卡：优先 ksu.spawn（命令在后台线程跑，不堵 WebView 主线程），没有时才退回同步的 ksu.exec
if (!/canSpawn\(\) \? await runSpawn\(cmd\) : await exec\(cmd\)/.test(main)) fail("xr() 应优先用 spawn");
// 状态刷新单飞；设置乐观更新（先改本地再后台保存），保存后合并刷新，不在点击路径上等 status-json
if (!/if \(refreshing\) \{ refreshAgain = true; return refreshing; \}/.test(main)) fail("refresh 应单飞");
{ const sk = main.slice(main.indexOf("async function setKey"), main.indexOf("// ---- 渲染 ----"));
  if (sk.indexOf("render(status)") < 0 || sk.indexOf("render(status)") > sk.indexOf('await xr("set"') || /await refresh\(\)/.test(sk)) fail("setKey 应乐观更新、不等 refresh"); }
if (!/for \(const \[k, e\] of edits\) if \(e\.doneAt !== null && t0 > e\.doneAt\) edits\.delete\(k\);/.test(main)) fail("保存前发出的刷新不能把开关弹回去");
if (!/requestAnimationFrame\(\(\) => setTimeout\(loadTraffic, 0\)\)/.test(main)) fail("切页后再读流量");
// 单次在途：先 await 本次 exec，再排下一次；开机期间暂停常规状态轮询
{ const bt = main.slice(main.indexOf("async function bootTick"), main.indexOf("// ---- 操作 ----"));
  if (bt.indexOf('await xr("boot-tail"') < 0 || bt.indexOf('await xr("boot-tail"') > bt.indexOf("setTimeout(() => bootTick(b), BOOT_POLL)")) fail("boot-tail 必须单次在途");
  if (!/!document\.hidden && !boot\) \{ await refresh\(\)/.test(main)) fail("开机期间应暂停常规状态轮询"); }
const blk = (sel) => css.slice(css.indexOf(sel + " {"), css.indexOf("}", css.indexOf(sel + " {")));
const plog = blk(".boot-log");
if (/blur|box-shadow|animation|filter/.test(plog + blk(".boot"))) fail("开机日志不得用模糊/阴影/动画");
if (!/\.boot-log\.glide \{ transition: transform \.1\d+s/.test(css)) fail("上移只用短 transform 过渡");
if (!/const BOOT_ROWS = \d+;/.test(main) || !/Array\.from\(\{ length: BOOT_ROWS \}/.test(main)) fail("应使用固定行池");
if (/getBoundingClientRect|offsetHeight|scrollHeight/.test(main.slice(main.indexOf("function bootPaint"), main.indexOf("async function bootTick")))) fail("绘制开机日志时不得读布局");
const bt = L.parseBootTail("R1\x1f120\x1f2026-11-11 09:00:00 [INFO] [vm] 已启动 pid=1\n\x1f30\x1f[    0.000000] Booting Linux\n[ 0.1] half");
eq([bt.rc, bt.mod.size, bt.con.size], [1, 120, 30], "parseBootTail");
eq(L.parseBootTail("R-\x1f0\x1f\x1f0\x1f").rc, null, "parseBootTail 无退出码");
eq(L.parseBootTail("garbage"), null, "parseBootTail 非法");
// 固定热点网段前端预检（与手机侧 hs_subnet_valid 一致）
for (const v of ["", "10.233.0.1/24", "192.168.77.254/24", "172.20.0.1/16", "10.9.9.1/30"]) eq(L.hsSubnetCheck(v).ok, true, `hsSubnetCheck ${v}`);
for (const v of ["10.233.0.0/24", "10.233.0.255/24", "10.233.0.1/15", "10.233.0.1/31", "8.8.8.1/24", "100.64.0.1/24", "10.233.0.1", "10.233.0.300/24", "x"]) eq(L.hsSubnetCheck(v).ok, false, `hsSubnetCheck ${v}`);
eq((({ rc, st, mod, con }) => [rc, st, mod.size, con.size, con.text])(L.parseBootTail("R-\x1fSbooting\x1f5\x1fabc\x1f9\x1fx\x1fy")), [null, "booting", 5, 9, "xy"], "parseBootTail 带虚拟机状态");
const bb = L.newBootBuf();
eq(L.feedBoot(bb, "mod", bt.mod.text), true, "feedBoot 有新行");
L.feedBoot(bb, "con", bt.con.text);
eq(bb.lines, ["[vm] 已启动 pid=1", "[    0.000000] Booting Linux"], "feedBoot 去时间前缀、保留半行");
L.feedBoot(bb, "con", " line\n\x1b[0m\x1b[1;32mok\x1b[0m\n");
eq(bb.lines.slice(-2), ["[ 0.1] half line", "ok"], "feedBoot 拼接半行并去颜色码");
for (let i = 0; i < 50; i++) L.feedBoot(bb, "con", "x".repeat(200) + "\n");
eq([bb.lines.length, bb.lines[0].length], [L.BOOT_LINES, L.BOOT_COLS], "feedBoot 行数与列宽上限");
eq(bb.total, 54, "feedBoot 累计行数");
eq(L.feedBoot(bb, "con", ""), false, "feedBoot 空");
// 启动中：电源按钮整个隐藏、没有转圈。舞台三态：未启动红色断电电路板（静态）、开机中只有日志、运行中绿色电路板 + 电流
if (!/\.starting\.power-wrap \{ display: none; \}/.test(css)) fail("启动中应隐藏电源按钮");
if (/\.starting[^{]*\.ring/.test(css)) fail("启动中不应有转圈动画");
// 底图：src/pcb.css（scripts/gen-pcb.py 生成的静态 SVG）。红色版走线断开（路径段数多于绿色版），绿色版接通
const pcb = fs.readFileSync(W + "/src/pcb.css", "utf8");
const pblk = (sel) => pcb.slice(pcb.indexOf(sel + " {"), pcb.indexOf("}\n", pcb.indexOf(sel + " {")));
const stg = pblk(".stage"); const stgRun = pblk(".stage.running");
const svgOf = (b) => { const m = /url\("data:image\/svg\+xml,([^"]+)"\)/.exec(b); return m ? decodeURIComponent(m[1]) : ""; };
const segs = (b) => (svgOf(b).match(/ ?M[-\d.]+ [-\d.]+/g) || []).length;
if (!/%23ff6b5e/.test(stg) || /animation|transition/.test(stg + stgRun)) fail("未启动：红色静态电路板");
if (!/%2357e389/.test(stgRun)) fail("运行中：绿色电路板");
if (!(segs(stg) > segs(stgRun) * 1.8)) fail("未启动的走线应断开");
for (const t of ["<pattern", "stroke-dasharray", "<circle", "rx='12'"]) if (!svgOf(stgRun).includes(t)) fail(`电路板应含 ${t}`);
if (!/import "\.\/pcb\.css";/.test(main) || !fs.existsSync(W + "/scripts/gen-pcb.py")) fail("电路板底图应由 gen-pcb.py 生成并引入");
if (!/\.stage\.starting \{[^}]*background-image: none/.test(css) || !/\.pcb-flow \{[^}]*display: none; \}/.test(css)) fail("开机中只显示日志");
if (!/cls\(\$\("stage"\), "stage " \+ v\.phase\)/.test(main)) fail("舞台应随启动阶段切换");
// 电流动画：只在运行中、只用 transform（合成层），页面不可见 / 不在首页时暂停；无发光滤镜；元素启动时一次建好
const kf = (n) => css.slice(css.indexOf(`@keyframes ${n}`), css.indexOf("}\n", css.indexOf(`@keyframes ${n}`)) + 1);
{ const k = kf("pcbFlow"); if (!k || /opacity|left|top|width|height|stroke|filter|shadow|background/.test(k.replace(/@keyframes \w+/, ""))) fail("pcbFlow 只能改 transform"); }
if (!/\.running \.pcb-flow \{ display: block; \}/.test(css)) fail("电流只在运行中流动");
if (/filter|box-shadow|blur/.test(blk(".pf i"))) fail("电流不能发光");
if (!/\.page:not\(\.on\) \.pf i, \.hid \.pf i \{ animation-play-state: paused; \}/.test(css) || !/classList\.toggle\("hid", document\.hidden\)/.test(main)) fail("不可见时应暂停电流动画");
{ const n = (fs.readFileSync(W + "/src/pcb.js", "utf8").match(/\{x:/g) || []).length; if (n < 4 || n > 12) fail("电流走线数量应在 4-12"); }
// 性能：状态刷新只在变化时写 DOM；日志内容未变不重写；不保留装饰性动画
if (!/const txt = \(el, v\) =>/.test(main) || /\$\("v-uptime"\)\.textContent =/.test(main)) fail("状态刷新应只在变化时写 DOM");
if (/requestAnimationFrame/.test(main) === false) fail("启动日志应按帧合并写入");
if (/@keyframes drop|\.switching/.test(css)) fail("不得保留装饰性过渡动画");
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
