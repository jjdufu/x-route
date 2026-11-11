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
for (const s of subs) if (!new RegExp(`^  ${s}\\)`, "m").test(xrsh)) fail(`xr 不支持子命令 ${s}`);
for (const s of ["status-json", "start", "stop", "set", "ipv6", "vpn-import", "ls", "log", "log-export", "log-clear", "expand", "reset-disk", "ack-trip", "luci"]) if (!subs.has(s)) fail(`WebUI 未使用 ${s}`);

// 布局与文案
const tabs = [...html.matchAll(/<md-primary-tab[^>]*>.*?<\/md-icon>([^<]+)<\/md-primary-tab>/g)].map((m) => m[1].trim());
eq(tabs, ["主页", "组网", "日志", "设置"], "顶部 Tab");
if (/bottom|tabbar|nav class/.test(html)) fail("不应有底部导航");
if (html.indexOf('class="appbar"') > html.indexOf('id="pager"')) fail("标题栏必须在内容区之外");
const css = fs.readFileSync(W + "/src/style.css", "utf8");
if (!html.includes("viewport-fit=cover")) fail("缺少 viewport-fit=cover");
if (!css.includes("safe-area-inset-top") || !css.includes("safe-area-inset-bottom")) fail("缺少 safe-area 变量");
if (css.includes("breathe") || /animation:\s*breathe/.test(css) || css.includes("@keyframes breathe")) fail("不得保留呼吸光动画");
if (css.includes("backdrop-filter") || /filter:\s*blur/.test(css)) fail("不得使用大面积模糊");
if (html.includes("概览") || main.includes("概览") || css.includes("概览")) fail("页签名应为「主页」");
for (const t of ["开机自启", "运行时长", "上游网络", "打开 LuCI", "导入配置", "连接状态", "路由网段", "运行模式", "管理访问", "IPv6",
  "存储共享", "目录", "虚拟机资源", "CPU", "内存", "系统盘", "扩容", "重置", "选择此文件夹", "导出当前", "导出全部", "整机复位后自启已关闭"]) {
  if (!html.includes(t)) fail(`缺少文案 ${t}`);
}
for (const t of ["谁可以访问", "互联", "上传并启用", "已接入的网段", "共享手机存储", "仅本机", "热点设备", "当前局域网", "控制台", "控制通道", "虚拟机管理器", "SSH"]) {
  if (html.includes(t) || main.includes(t)) fail(`旧文案残留 ${t}`);
}
const visible = html.replace(/<script[\s\S]*?<\/script>/g, "").replace(/<[^>]+>/g, " ");
if (/XR_|VM_|AUTOSTART|hotspot|bypass|global|ovpn|SWIOTLB/.test(visible)) fail("页面可见文案含内部变量名或取值");
eq(L.MODE_OPTS.map((o) => o.label), ["全局接管", "旁路网关"], "运行模式");
eq(L.ACCESS_OPTS.map((o) => o.label), ["本机", "内网", "所有"], "管理访问");
eq(L.SHARE_OPTS.map((o) => o.label), ["只读", "读写"], "权限");
eq(L.LOG_KINDS.map((o) => o.label), ["模块", "串口", "控制服务", "虚拟机"], "日志分类");
const exposeVals = common.match(/XR_EXPOSE\)\n\s*case "\$2" in ([\w|]+)\)/)[1].split("|");
eq(L.ACCESS_OPTS.map((o) => o.value), exposeVals, "管理访问取值与 valid_kv 一致");
eq(L.MODE_OPTS.map((o) => o.value), ["global", "bypass"], "运行模式取值");
if (!/XR_EXPOSE="lan"/.test(fs.readFileSync(root + "/module/config/config.conf", "utf8"))) fail("管理访问默认应为内网");
for (const k of L.SET_KEYS) if (!new RegExp(`\\b${k}\\b`).test(common)) fail(`SET_KEYS 含未知键 ${k}`);

// 确认弹窗只用于：运行模式、停止、重置 / 扩容系统盘、清空日志
const confirms = [...main.matchAll(/confirmDlg\([`"]([^`"]+)/g)].map((m) => m[1]);
eq(confirms.sort(), ["切换为${label}", "停止虚拟机", "清空${label}日志", "重置系统盘"].sort(), "确认弹窗范围");
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
