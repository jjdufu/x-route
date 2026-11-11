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
[ "\$1" = status ] && printf '{"mode":"ndc","up_if":"wlan0","expose":"lan","expose_if":["wlan1"],"vm_ip":"172.29.233.254","host_ip":"172.29.233.1"}\n'
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
chk '$XR get | grep -qx "XR_EXPOSE=lan" && [ "$($XR get | wc -l)" -eq 23 ]'
cp "$CONF" "$T/conf.bak"
# 通知显示：取值校验，改后不需重启，并请求守护进程立即刷新通知
rm -f "$RUN/notify.force"
chk '$XR set XR_NOTIFY_TRAFFIC wan | grep -qx "{\"ok\":true,\"restart\":false}" && grep -qx "XR_NOTIFY_TRAFFIC=\"wan\"" "$CONF" && [ -f "$RUN/notify.force" ]'
chk '! $XR set XR_NOTIFY_TRAFFIC speed >/dev/null && grep -qx "XR_NOTIFY_TRAFFIC=\"wan\"" "$CONF"'
$XR set XR_NOTIFY_TRAFFIC total >/dev/null
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
chk '$XR status-line | grep -q "^已停止 / - / 172.29.233.254 / 管理访问=lan / VPN网段=0$"'
chk '! $XR running'
# 启动 -> 稳定 -> 自启允许；需重启键提示 restart=true
$XR start; chk '[ $? -eq 0 ] && $XR running'
chk '[ "$($XR set XR_DNS "1.1.1.1")" = "{\"ok\":true,\"restart\":true}" ]'
$XR set XR_DNS "223.5.5.5 119.29.29.29" >/dev/null
sleep 10
chk '$XR status-line | grep -q "^运行中 / 运行中 /"'
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
# IPv6：客侧未上报时为 null 且不能切换；上报后读实际状态；切换写控制命令、等客侧确认、再同步手机侧
chk 'echo "$s" | J "d[\"ipv6\"] is None" | grep -qx True'
out="$(XR_IPV6_WAIT=1 $XR ipv6 on)"; chk '[ $? -ne 0 ] && echo "$out" | grep -q "虚拟机未就绪"'
echo 0 > "$RUN/guest/guest/ipv6"
chk '$XR status-json | J "d[\"ipv6\"] is False" | grep -qx True'
( sleep 2; echo 1 > "$RUN/guest/guest/ipv6" ) &
: > "$T/net.calls"; out="$($XR ipv6 on)"
chk '[ "$out" = "{\"ok\":true}" ] && grep -q "^on [0-9]*$" "$RUN/guest/host/ipv6.cmd" && grep -qx "net sync" "$T/net.calls"'
chk '$XR status-json | J "d[\"ipv6\"] is True" | grep -qx True'
out="$(XR_IPV6_WAIT=1 $XR ipv6 off)"; chk '[ $? -ne 0 ] && echo "$out" | grep -q "虚拟机未响应" && grep -q "^off " "$RUN/guest/host/ipv6.cmd"'
out="$($XR ipv6 maybe)"; chk '[ $? -eq 2 ]'
: > "$T/net.calls"; $XR sync >/dev/null
# 从内部存储导入占位 ovpn（无密钥）；运行中写入控制目录（带时间戳，重复导入也会重载），不得写入日志
export XR_MEDIA="$T/media"; mkdir -p "$XR_MEDIA/Download/vpn" "$XR_MEDIA/.hidden" "$XR_MEDIA/Music"
printf 'client\ndev tun\nremote 127.0.0.1 1194\n' > "$XR_MEDIA/Download/vpn/home.ovpn"
printf 'x' > "$XR_MEDIA/Download/vpn/notes.txt"; printf 'abc\001\002defghij' > "$XR_MEDIA/Download/vpn/bin.conf"
chk '[ "$($XR ls "$XR_MEDIA" | tr "\n" " ")" = "d/Download d/Music " ]'
chk '[ "$($XR ls /storage/emulated/0/Download/vpn ovpn | tr "\n" " ")" = "f/bin.conf f/home.ovpn " ] && [ -z "$($XR ls /storage/emulated/0/Download/vpn)" ]'
chk '! $XR ls /data/adb >/dev/null && ! $XR ls "$XR_MEDIA/../" >/dev/null'
chk '$XR vpn-import /storage/emulated/0/Download/vpn/home.ovpn | grep -q "\"ok\":true"'
chk 'grep -q "remote 127.0.0.1 1194" "$RUN/guest/host/openvpn.ovpn" && grep -q "^enable [0-9]*$" "$RUN/guest/host/openvpn.cmd" && [ -f "$XR_DATA/config/openvpn.ovpn" ]'
chk '$XR vpn-import /storage/emulated/0/Download/vpn/bin.conf | grep -q "\"ok\":false" && ! $XR vpn-import /data/adb/x.ovpn >/dev/null'
chk '! grep -q "127.0.0.1 1194" "$LOG"'
chk '$XR set XR_EXPOSE all >/dev/null && [ "$(grep -c "net sync" "$T/net.calls")" -eq 2 ]'
# 日志统一：不再有分类查看 / 清空命令
out="$($XR log xr 5)"; chk '[ $? -eq 2 ]'; out="$($XR log-clear all)"; chk '[ $? -eq 2 ]'
# 日志导出：普通日志 .tar.gz（文件名带日期时间，脱敏），调试开启时诊断导出另含快照与调试日志
mkdir -p "$T/dl"
export XR_DOWNLOAD="$T/dl"
printf 'module-body password=hunter2\n' > "$LOG"
printf 'console-body\n' > "$RUN/console.log"
printf 'ctl-body\n' > "$RUN/vm-ctl.log"
printf 'vmm-body\n' > "$XR_DATA/log/vmm.log"
printf 'dbg-body\n' > "$XR_DATA/log/debug.log"
out="$($XR log-export)"
chk 'echo "$out" | grep -q "\"ok\":true"'
tgz="$(echo "$out" | python3 -c "import sys,json; print(json.load(sys.stdin)[\"path\"])")"
chk 'echo "$tgz" | grep -qE "/xrouter-logs-[0-9]{8}-[0-9]{6}\.tar\.gz$" && [ -f "$tgz" ]'
python3 - "$tgz" <<'PYZ' || fail=1
import sys, tarfile, os
t = tarfile.open(sys.argv[1], "r:gz")
base = os.path.basename(sys.argv[1])[:-7]
names = {m.name for m in t.getmembers() if m.isfile()}
need = {f"{base}/{n}" for n in ("summary.txt", "module.log", "console.log", "vm-ctl.log", "vmm.log")}
if names != need:
    print("FAIL: 普通日志包内文件", names); sys.exit(1)
m = t.extractfile(f"{base}/module.log").read()
if b"module-body" not in m or b"hunter2" in m:
    print("FAIL: module.log 内容或脱敏", m); sys.exit(1)
PYZ
out="$($XR diag-export)"
chk 'echo "$out" | grep -q "\"ok\":true"'
dg="$(echo "$out" | python3 -c "import sys,json; print(json.load(sys.stdin)[\"path\"])")"
chk 'echo "$dg" | grep -qE "/xrouter-diag-[0-9]{8}-[0-9]{6}\.tar\.gz$"'
chk 'tar -tzf "$dg" | grep -q "/debug.log$" && tar -tzf "$dg" | grep -q "/phone.txt$" && tar -tzf "$dg" | grep -q "/module.log$"'
$XR stop; chk '! $XR running'
# 熔断：存在 tripped 时自启和启动都被拒绝，ack-trip 后恢复
$XR set AUTOSTART 0 >/dev/null; date > "$TRIPPED"
out="$($XR set AUTOSTART 1)"; chk '[ $? -ne 0 ] && echo "$out" | grep -q "熔断" && grep -qx "AUTOSTART=\"0\"" "$CONF"'
chk '$XR status-json | J "d[\"vm\"][\"tripped\"] is not None" | grep -qx True'
$XR start; chk '[ $? -ne 0 ] && ! $XR running'
$XR ack-trip; chk '[ ! -f "$TRIPPED" ]'
# WebUI 异步启停：start-bg 立即返回，真正启动在后台完成并写 bg.rc
echo old-boot > "$RUN/console.log"
t0="$(date +%s)"; out="$($XR start-bg)"; t1="$(date +%s)"
chk '[ "$out" = "{\"ok\":true}" ] && [ $((t1 - t0)) -le 1 ]'
chk '! grep -q old-boot "$RUN/console.log"'
i=0; while [ ! -f "$RUN/bg.rc" ] && [ $i -lt 60 ]; do sleep 0.25; i=$((i+1)); done
chk '[ "$(cat "$RUN/bg.rc")" = 0 ] && $XR running'
chk '$XR boot-tail -1 -1 | python3 -c "import sys; f=sys.stdin.read().split(chr(31)); sys.exit(0 if f[0]==\"R0\" else 1)"'
out="$($XR stop-bg)"; chk '[ "$out" = "{\"ok\":true}" ]'
i=0; while $XR running && [ $i -lt 80 ]; do sleep 0.25; i=$((i+1)); done
chk '! $XR running'
i=0; while [ ! -f "$RUN/bg.rc" ] && [ $i -lt 40 ]; do sleep 0.25; i=$((i+1)); done
chk '[ "$(cat "$RUN/bg.rc")" = 0 ]'
# boot-tail：按字节偏移增量读取、截断后从头读、去掉控制字符、超长只给末尾 6KB
BT() { $XR boot-tail "$@" | python3 -c "import sys; f=sys.stdin.buffer.read().split(b'\x1f'); print(\"|\".join(x.decode().replace(chr(10), \"/\") for x in f))"; }
printf 'l1\nl2\n' > "$RUN/console.log"; : > "$LOG"
chk '[ "$(BT -1 -1)" = "R0|Sstopped|0||6|" ]'
chk '[ "$(BT 0 0)" = "R0|Sstopped|0||6|l1/l2/" ]'
printf 'l3\033[0m\r\n' >> "$RUN/console.log"
chk '[ "$(BT 0 6)" = "R0|Sstopped|0||14|l3[0m/" ]'
chk '[ "$(BT 0 12345678)" = "R0|Sstopped|0||14|l1/l2/l3[0m/" ]'
head -c 10000 /dev/zero | tr '\0' a > "$RUN/console.log"
chk '[ "$($XR boot-tail -1 0 | python3 -c "import sys; print(len(sys.stdin.buffer.read().split(b\"\x1f\")[5]))")" = 6144 ]'
rm -f "$RUN/bg.rc"; chk '$XR boot-tail -1 -1 | grep -q "^R-"'
echo ready > "$RUN/state"; chk '[ "$(BT -1 -1 | cut -d"|" -f2)" = Sready ]'; rm -f "$RUN/state"
# 快速路径：boot-tail 不加载其余脚本（不 source common.sh），开机期间每 ~200ms 调用也很轻
chk '[ "$(sed -n "/^if \[ \"\${1:-}\" = boot-tail \]/,/^fi/p" "$ROOT/module/scripts/xr" | grep -c "^\. ")" = 0 ]'
chk '[ "$(grep -n "= boot-tail \]" "$ROOT/module/scripts/xr" | cut -d: -f1)" -lt "$(grep -n "scripts/common.sh\"$" "$ROOT/module/scripts/xr" | head -1 | cut -d: -f1)" ]'
# 用法错误
out="$($XR bogus)"; chk '[ $? -eq 2 ]'
out="$($XR expand abc)"; chk '[ $? -eq 2 ]'
if [ "$fail" = 0 ]; then echo "test-xr OK"; else echo "test-xr FAILED"; exit 1; fi
