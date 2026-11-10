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
chk '$XR get | grep -qx "XR_EXPOSE=hotspot" && [ "$($XR get | wc -l)" -eq 20 ]'
cp "$CONF" "$T/conf.bak"
out="$($XR set VM_MEM_MB 99999)"; chk '[ $? -ne 0 ] && echo "$out" | grep -q "\"ok\":false" && cmp -s "$CONF" "$T/conf.bak"'
out="$($XR set 'X;rm' 1)"; chk '[ $? -ne 0 ] && cmp -s "$CONF" "$T/conf.bak"'
out="$($XR set VPN_ROUTES "8.8.8.0/24")"; chk '[ $? -ne 0 ] && cmp -s "$CONF" "$T/conf.bak"'
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
# VPN_ROUTES：运行中立即 net.sh sync；被拒绝的网段出现在 routes_rejected
: > "$T/net.calls"
chk '[ "$($XR set VPN_ROUTES "10.8.0.0/24,192.168.9.0/24")" = "{\"ok\":true,\"restart\":false}" ] && grep -qx "net sync" "$T/net.calls"'
s="$($XR status-json)"
chk 'echo "$s" | J "d[\"vpn\"][\"routes_active\"]==[\"10.8.0.0/24\"] and d[\"vpn\"][\"routes_rejected\"]==[\"192.168.9.0/24\"] and d[\"vm\"][\"running\"] is True and d[\"vm\"][\"last_good_match\"] is True" | grep -qx True'
chk '$XR set XR_EXPOSE all >/dev/null && [ "$(grep -c "net sync" "$T/net.calls")" -eq 2 ]'
chk '$XR log xr 5 | grep -q . && [ "$($XR log xr 3 | wc -l)" -eq 3 ] && $XR log console >/dev/null'
out="$($XR log bogus)"; chk '[ $? -eq 2 ]'
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
