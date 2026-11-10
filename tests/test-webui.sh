#!/bin/sh
# XRouter: WebUI 静态检查（需要 node；没有 node 时跳过）
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
W="$ROOT/module/webroot"
command -v node >/dev/null 2>&1 || { echo "test-webui skip (no node)"; exit 0; }
fail=0
if grep -nE '192\.168\.100|5244|keepalive|alist|cat <<|CONF_FILE|config\.conf|已稳定|VPN_MODE|VPN_ROUTES' "$W"/*; then echo "FAIL: 旧 UI 残留"; fail=1; fi
if grep -nE '<select|window\.confirm' "$W"/*; then echo "FAIL: 使用了系统 select/confirm"; fail=1; fi
if grep -nE '(^|[^[:alnum:]_])confirm[[:space:]]*\(' "$W/app.js"; then echo "FAIL: 使用了系统 confirm"; fail=1; fi
if grep -n 'word-break:[[:space:]]*break-all' "$W/style.css"; then echo "FAIL: word-break: break-all"; fail=1; fi
node --check "$W/app.js" || fail=1
node - "$W" <<'EOS' || fail=1
const fs = require("fs"); const vm = require("vm"); const dir = process.argv[2];
const html = fs.readFileSync(dir + "/index.html", "utf8"); const js = fs.readFileSync(dir + "/app.js", "utf8");
let bad = 0; const fail = (m) => { console.log("FAIL: " + m); bad = 1; };
for (const m of js.matchAll(/\$\("([\w-]+)"\)/g)) if (!html.includes(`id="${m[1]}"`)) fail(`index.html 缺少 id=${m[1]}`);
const keys = (js.match(/const SET_KEYS = \[([\s\S]*?)\]/)[1].match(/"(\w+)"/g) || []).map((s) => s.slice(1, -1));
for (const m of html.matchAll(/data-key="(\w+)"/g)) if (!keys.includes(m[1])) fail(`data-key ${m[1]} 不在 SET_KEYS`);
const pages = ["home", "link", "logs", "settings"];
for (const p of pages) if (!html.includes(`id="page-${p}"`)) fail(`缺少 page-${p}`);
for (const label of ["主页", "互联", "日志", "设置"]) if (!html.includes(label)) fail(`底部导航缺少 ${label}`);
for (const label of ["模块", "控制台", "控制通道", "虚拟机管理器"]) if (!html.includes(label)) fail(`日志分类缺少 ${label}`);
if (!html.includes("运行模式") || !html.includes(">全局<") || !html.includes(">旁路<")) fail("缺少运行模式切换");
if (html.includes("SSH") || html.includes("ssh")) fail("不应显示 SSH");
const visible = html.replace(/<script[\s\S]*?<\/script>/g, "").replace(/<[^>]+>/g, " ");
if (/XR_|VPN_MODE|VPN_ROUTES|hotspot：|terminal：|SWIOTLB|STABLE_SEC/.test(visible)) fail("页面可见文案含内部变量名或前缀");
if (!html.includes('id="modal"') || !js.includes("confirmDlg")) fail("缺少自定义确认弹窗");
const src = js.split("\nbind();")[0].replace('"use strict";', "");
const ctx = { window: {}, document: { getElementById: () => null } }; vm.createContext(ctx); vm.runInContext(src + ";this.shq=shq;this.vmLabel=vmLabel;this.fmtUptime=fmtUptime;this.UP_KIND=UP_KIND;", ctx);
const eq = (a, b, m) => { if (a !== b) fail(`${m}: ${JSON.stringify(a)} != ${JSON.stringify(b)}`); };
eq(ctx.shq("a'b"), "'a'\\''b'", "shq");
eq(ctx.shq("$(reboot)"), "'$(reboot)'", "shq 不展开");
eq(ctx.vmLabel({ running: false }), "已停止", "停止");
eq(ctx.vmLabel({ running: true, state: "starting" }), "启动中", "启动中");
eq(ctx.vmLabel({ running: true, state: "ready" }), "运行中", "ready");
eq(ctx.vmLabel({ running: true, state: "stable" }), "运行中", "stable 仍显示运行中");
eq(ctx.fmtUptime(65), "1 分 5 秒", "uptime");
eq(ctx.UP_KIND("wlan0"), "Wi-Fi", "wifi");
eq(ctx.UP_KIND("rmnet_data0"), "蜂窝", "cell");
eq(ctx.UP_KIND(""), "无", "empty");
process.exit(bad);
EOS
if [ "$fail" = 0 ]; then echo "test-webui OK"; else echo "test-webui FAILED"; exit 1; fi
