#!/bin/sh
# x-route: WebUI 静态检查（需要 node；没有 node 时跳过）
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
W="$ROOT/module/webroot"
command -v node >/dev/null 2>&1 || { echo "test-webui skip (no node)"; exit 0; }
fail=0
if grep -nE '192\.168\.100|5244|keepalive|alist|cat <<|CONF_FILE|config\.conf' "$W"/*; then echo "FAIL: 旧 UI 残留"; fail=1; fi
node --check "$W/app.js" || fail=1
node - "$W" <<'EOS' || fail=1
const fs = require("fs"); const vm = require("vm"); const dir = process.argv[2];
const html = fs.readFileSync(dir + "/index.html", "utf8"); const js = fs.readFileSync(dir + "/app.js", "utf8");
let bad = 0; const fail = (m) => { console.log("FAIL: " + m); bad = 1; };
for (const m of js.matchAll(/\$\("([\w-]+)"\)/g)) if (!html.includes(`id="${m[1]}"`)) fail(`index.html 缺少 id=${m[1]}`);
const keys = (js.match(/const SET_KEYS = \[([\s\S]*?)\]/)[1].match(/"(\w+)"/g) || []).map((s) => s.slice(1, -1));
for (const m of html.matchAll(/data-key="(\w+)"/g)) if (!keys.includes(m[1])) fail(`data-key ${m[1]} 不在 SET_KEYS`);
// 只取纯函数出来测（不执行 bind/refresh）
const src = js.split("\nbind();")[0].replace('"use strict";', "");
const ctx = { window: {}, document: { getElementById: () => null } }; vm.createContext(ctx); vm.runInContext(src + ";this.shq=shq;this.cidrOk=cidrOk;", ctx);
const eq = (a, b, m) => { if (a !== b) fail(`${m}: ${JSON.stringify(a)} != ${JSON.stringify(b)}`); };
eq(ctx.shq("a'b"), "'a'\\''b'", "shq");
eq(ctx.shq("$(reboot)"), "'$(reboot)'", "shq 不展开");
for (const c of ["10.20.0.0/16", "172.16.0.0/12", "192.168.50.0/24", "100.64.0.0/10", "10.8.0.0/24"]) eq(ctx.cidrOk(c), "", c);
eq(ctx.cidrOk("8.8.8.0/24"), "不是私网地址", "8.8.8.0/24");
eq(ctx.cidrOk("192.168.50.1/24"), "主机位不为 0", "192.168.50.1/24");
eq(ctx.cidrOk("10.0.0.0/7"), "格式错误", "/7");
eq(ctx.cidrOk("10.0.0.0/33"), "格式错误", "/33");
eq(ctx.cidrOk("172.32.0.0/16"), "不是私网地址", "172.32");
eq(ctx.cidrOk("300.1.1.0/24"), "格式错误", "300");
eq(ctx.cidrOk("a;b"), "格式错误", "a;b");
process.exit(bad);
EOS
if [ "$fail" = 0 ]; then echo "test-webui OK"; else echo "test-webui FAILED"; exit 1; fi
