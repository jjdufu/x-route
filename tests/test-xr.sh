#!/bin/sh
# shellcheck disable=SC2016,SC2317,SC2034
# XRouter: xr 命令行测试（假 crosvm / 假 net.sh / 假 getprop）
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
T="$(mktemp -d)"
fail=0; chk() { if eval "$1"; then :; else echo "FAIL: $1"; fail=1; fi; }
mkdir -p "$T/bin" "$T/mod"
cp -a "$ROOT/module/." "$T/mod/"
printf '#!/bin/sh\ncase "$1" in sys.boot_completed) echo 1;; sys.user.0.ce_available) echo true;; esac\n' > "$T/bin/getprop"; chmod +x "$T/bin/getprop"
# 假 net.sh：记录调用；status 输出固定 JSON
cat > "$T/mod/scripts/net.sh" <<EOS
#!/bin/sh
echo "net \$*" >> "$T/net.calls"
[ "\$1" = status ] && printf '{"mode":"ndc","up_if":"wlan0","expose":"hotspot","expose_if":["wlan1"],"vm_ip":"172.29.233.254","host_ip":"172.29.233.1"}\n'
[ "\$1" = sync ] && printf '10.8.0.0/24\n' > "$T/data/run/routes.active"
exit 0
EOS
export XR_DAEMON_FOREGROUND=1 PATH="$T/bin:$PATH" XR_DATA="$T/data" MODDIR="$T/mod" CROSVM="$ROOT/tests/mock/crosvm" FAKE_ARGS="$T/args"
XR="sh $T/mod/scripts/xr"
trap '$XR stop >/dev/null 2>&1; rm -rf "$T"' EXIT
. "$T/mod/scripts/common.sh"; ensure_dirs
cp "$T/mod/config/config.conf" "$CONF"
sed -i 's/^READY_TIMEOUT_SEC=.*/READY_TIMEOUT_SEC="6"/; s/^STABLE_SEC=.*/STABLE_SEC="5"/' "$CONF"
echo k > "$IMG/Image"; truncate -s 300M "$ROOTFS"
J() { python3 -c "import json,sys; d=json.load(sys.stdin); print($1)"; }

# get / set
chk '$XR get | grep -qx "XR_EXPOSE=hotspot" && [ "$($XR get | wc -l)" -eq 19 ]'
cp "$CONF" "$T/conf.bak"
out="$($XR set VM_MEM_MB 99999)"; chk '[ $? -ne 0 ] && echo "$out" | grep -q "\"ok\":false" && cmp -s "$CONF" "$T/conf.bak"'
out="$($XR set 'X;rm' 1)"; chk '[ $? -ne 0 ] && cmp -s "$CONF" "$T/conf.bak"'
out="$($XR set VPN_ROUTES "8.8.8.0/24")"; chk '[ $? -ne 0 ] && cmp -s "$CONF" "$T/conf.bak"'
# 未知键应被拒绝（已删除手工网段）
out="$($XR set VM_MEM_MB)"; chk '[ $? -eq 2 ]'
chk '[ "$($XR set VM_MEM_MB 768)" = "{\"ok\":true,\"restart\":false}" ] && grep -qx "VM_MEM_MB=\"768\"" "$CONF"'
# AUTOSTART 未手动成功启动过 -> 拒绝并保持 0
out="$($XR set AUTOSTART 1)"; chk '[ $? -ne 0 ] && echo "$out" | grep -q "手动启动成功" && grep -qx "AUTOSTART=\"0\"" "$CONF"'
# status 停止态
s="$($XR status-json)"
chk 'echo "$s" | J "d[\"vm\"][\"running\"] is False and d[\"vm\"][\"state\"]==\"stopped\" and d[\"share\"][\"mode\"]==\"off\" and d[\"config\"][\"VM_MEM_MB\"]==\"768\" and d[\"net\"][\"mode\"]==\"ndc\"" | grep -qx True'
chk '$XR status-line | grep -q "^已停止 / - / 172.29.233.254 / 暴露=hotspot / VPN网段=0$"'
chk '! $XR running'
# 启动 -> 稳定 -> 自启允许；需重启键提示 restart=true
$XR start; chk '[ $? -eq 0 ] && $XR running'
chk '[ "$($XR set XR_DNS "1.1.1.1")" = "{\"ok\":true,\"restart\":true}" ]'
$XR set XR_DNS "223.5.5.5 119.29.29.29" >/dev/null
sleep 10
chk '$XR status-line | grep -q "^运行中 / 已稳定 /"'
chk '[ "$($XR set AUTOSTART 1)" = "{\"ok\":true,\"restart\":false}" ] && grep -qx "AUTOSTART=\"1\"" "$CONF"'
# 客侧上报路由：sync 后读入；被拒绝的网段出现在 routes_rejected
: > "$T/net.calls"
mkdir -p "$RUN/guest/guest" "$RUN/guest/host"
printf '%s\n' "10.8.0.0/24" "192.168.9.0/24" > "$RUN/guest/guest/openvpn.routes"
echo up > "$RUN/guest/guest/openvpn.status"
echo "10.8.0.5" > "$RUN/guest/guest/openvpn.addr"
$XR sync >/dev/null
chk 'grep -qx "net sync" "$T/net.calls"'
s="$($XR status-json)"
chk 'echo "$s" | J "d[\"vpn\"][\"status\"]==\"up\" and d[\"vpn\"][\"addr\"]==\"10.8.0.5\" and d[\"vpn\"][\"routes_active\"]==[\"10.8.0.0/24\"] and d[\"vpn\"][\"routes_rejected\"]==[\"192.168.9.0/24\"] and d[\"vm\"][\"running\"] is True and d[\"vm\"][\"last_good_match\"] is True" | grep -qx True'
# 分片导入占位 ovpn（无密钥）；运行中写入控制目录，不得写入日志
ovpn="$(printf 'client\ndev tun\nremote 127.0.0.1 1194\n')"
b64="$(printf '%s' "$ovpn" | base64 | tr -d '\n')"
chk '$XR vpn-stage-begin | grep -q "\"ok\":true"'
chk '$XR vpn-stage-append "$b64" | grep -q "\"ok\":true"'
chk '$XR vpn-stage-commit | grep -q "\"ok\":true"'
chk 'grep -q "remote 127.0.0.1 1194" "$RUN/guest/host/openvpn.ovpn" && grep -qx enable "$RUN/guest/host/openvpn.cmd"'
chk '! grep -q "127.0.0.1 1194" "$LOG"'
chk '$XR set XR_EXPOSE all >/dev/null && [ "$(grep -c "net sync" "$T/net.calls")" -eq 2 ]'
chk '$XR log xr 5 | grep -q . && [ "$($XR log xr 3 | wc -l)" -eq 3 ] && $XR log console >/dev/null'
out="$($XR log bogus)"; chk '[ $? -eq 2 ]'
# 日志导出：压缩包与单类文件名、内容
mkdir -p "$T/dl"
export XR_DOWNLOAD="$T/dl"
printf 'module-body\n' > "$LOG"
printf 'console-body\n' > "$RUN/console.log"
printf 'ctl-body\n' > "$RUN/vm-ctl.log"
printf 'vmm-body\n' > "$XR_DATA/log/vmm.log"
out="$($XR log-export all)"
chk 'echo "$out" | grep -q "\"ok\":true"'
zipf="$(echo "$out" | python3 -c "import sys,json; print(json.load(sys.stdin)[\"path\"])")"
chk 'echo "$zipf" | grep -qE "xrouter-logs-[0-9]{8}-[0-9]{6}\.zip$" && [ -f "$zipf" ]'
python3 - "$zipf" <<'PYZ' || fail=1
import sys, zipfile
z = zipfile.ZipFile(sys.argv[1])
names = set(z.namelist())
need = {"module.log", "console.log", "vm-ctl.log", "vmm.log"}
if names != need:
    print("FAIL: zip 内文件", names); sys.exit(1)
if z.read("module.log") != b"module-body\n":
    print("FAIL: module.log 内容"); sys.exit(1)
if z.read("console.log") != b"console-body\n":
    print("FAIL: console.log 内容"); sys.exit(1)
PYZ
out="$($XR log-export module)"
chk 'echo "$out" | grep -q "\"ok\":true"'
one="$(echo "$out" | python3 -c "import sys,json; print(json.load(sys.stdin)[\"path\"])")"
chk 'echo "$one" | grep -qE "module-[0-9]{8}-[0-9]{6}\.log$" && grep -qx module-body "$one"'
out="$($XR log-export bogus)"; chk '[ $? -eq 2 ] && echo "$out" | grep -q "\"ok\":false"'
$XR stop; chk '! $XR running'
# 熔断：存在 tripped 时自启和启动都被拒绝，ack-trip 后恢复
$XR set AUTOSTART 0 >/dev/null; date > "$TRIPPED"
out="$($XR set AUTOSTART 1)"; chk '[ $? -ne 0 ] && echo "$out" | grep -q "熔断" && grep -qx "AUTOSTART=\"0\"" "$CONF"'
chk '$XR status-json | J "d[\"vm\"][\"tripped\"] is not None" | grep -qx True'
$XR start; chk '[ $? -ne 0 ] && ! $XR running'
$XR ack-trip; chk '[ ! -f "$TRIPPED" ]'
# 用法错误
out="$($XR bogus)"; chk '[ $? -eq 2 ]'
out="$($XR expand abc)"; chk '[ $? -eq 2 ]'
if [ "$fail" = 0 ]; then echo "test-xr OK"; else echo "test-xr FAILED"; exit 1; fi
