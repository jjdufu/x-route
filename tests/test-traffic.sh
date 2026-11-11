#!/bin/sh
# shellcheck disable=SC2016,SC2317,SC2034
# XRouter: 流量统计（traffic.sh）模拟测试：假 iptables / ip6tables，计数由 $MOCK/cnt.<链> 提供
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
T="$(mktemp -d)"; MOCK="$T/mock"; mkdir -p "$MOCK" "$T/proc/sys/net/ipv4/conf/xr0" "$T/proc/sys/net/ipv6/conf/xr0" "$T/sysnet/xr0"
fail=0; chk() { if eval "$1"; then :; else echo "FAIL: $1"; fail=1; fi; }
trap 'rm -rf "$T"' EXIT
echo 0 > "$T/proc/sys/net/ipv4/ip_forward"
export PATH="$ROOT/tests/mock/net:$PATH" MOCK XR_DATA="$T/data" MODDIR="$ROOT/module" XR_SYSBIN="$ROOT/tests/mock/net" \
  XR_PROC="$T/proc" XR_SYSNET="$T/sysnet" XR_DAEMON_FOREGROUND=1
. "$ROOT/module/scripts/common.sh"; ensure_dirs
cp "$ROOT/module/config/config.conf" "$CONF"
NET="sh $ROOT/module/scripts/net.sh"
XR="sh $ROOT/module/scripts/xr"
J() { python3 -c "import json,sys; d=json.load(sys.stdin); print($1)"; }
echo "11000:	from all fwmark 0x0/0xffff iif lo lookup rmnet_data2" > "$MOCK/iprule"
mkdir -p "$RUN/guest/guest"
printf '%s\n' "10.8.0.0/24" "8.8.8.0/24" "2001:db8:10::/64" > "$RUN/guest/guest/openvpn.routes"

# $1=链 $2=内网字节（写在第一条） $3=公网字节（写在最后一条兜底规则）
setcnt() {
  n="$(wc -l < "$MOCK/chain.$1")"; i=1; : > "$MOCK/cnt.$1"
  while [ "$i" -le "$n" ]; do
    if [ "$i" -eq "$n" ]; then echo "$3"; elif [ "$i" -eq 1 ]; then echo "$2"; else echo 0; fi >> "$MOCK/cnt.$1"
    i=$((i + 1))
  done
}

# 1. up：建链、规则顺序、挂载点
$NET up
chk '[ -f "$MOCK/chain.XR_ACCT" ] && [ -f "$MOCK/chain.XR_ACCT_UP" ] && [ -f "$MOCK/chain.XR_ACCT_DN" ]'
chk '[ "$(cat "$MOCK/chain.XR_ACCT")" = "$(printf "%s\n" "-i xr0 -j XR_ACCT_UP" "-o xr0 -j XR_ACCT_DN")" ]'
chk '[ "$(tail -1 "$MOCK/chain.XR_ACCT_UP")" = "-j RETURN" ] && [ "$(tail -1 "$MOCK/chain.XR_ACCT_DN")" = "-j RETURN" ]'
for c in 10.0.0.0/8 172.16.0.0/12 192.168.0.0/16 100.64.0.0/10 169.254.0.0/16 224.0.0.0/4 10.8.0.0/24 8.8.8.0/24; do
  chk "grep -qx -- '-d $c -j RETURN' '$MOCK/chain.XR_ACCT_UP' && grep -qx -- '-s $c -j RETURN' '$MOCK/chain.XR_ACCT_DN'"
done
chk '! grep -q ":" "$MOCK/chain.XR_ACCT_UP"'
chk 'grep -qx -- "-d 2001:db8:10::/64 -j RETURN" "$MOCK/chain.XR_ACCT6_UP" && grep -qx -- "-d fc00::/7 -j RETURN" "$MOCK/chain.XR_ACCT6_UP"'
chk 'grep -qx -- "-s fe80::/10 -j RETURN" "$MOCK/chain.XR_ACCT6_DN" && [ "$(tail -1 "$MOCK/chain.XR_ACCT6_DN")" = "-j RETURN" ]'
chk '[ "$(sed -n 1p "$MOCK/XR_FWD")" = "-j XR_ACCT" ] && [ "$(sed -n 1p "$MOCK/XR_FWD6")" = "-j XR_ACCT6" ]'
chk 'grep -qx -- "-i xr0 -j XR_ACCT" "$MOCK/hook.INPUT" && grep -qx -- "-o xr0 -j XR_ACCT" "$MOCK/hook.OUTPUT"'
chk 'grep -qx -- "-i xr0 -j XR_ACCT6" "$MOCK/hook6.INPUT" && grep -qx -- "-o xr0 -j XR_ACCT6" "$MOCK/hook6.OUTPUT"'
# 只计数：统计链里只有 RETURN，不改包、不用 mangle
chk '! grep -h -v -- "-j RETURN$" "$MOCK/chain.XR_ACCT_UP" "$MOCK/chain.XR_ACCT_DN" | grep -q .'
chk '! grep -v "XR_MSS" "$MOCK/calls" | grep -q -- "-t mangle"'   # 统计不用 mangle（XR_MSS 是网络层的 MSS 钳制，不属于统计）

# 2. 读数：公网 / 内网 / IPv6 汇总；超过 32 位也不溢出
setcnt XR_ACCT_UP 1000 5000000000; setcnt XR_ACCT_DN 2000 9000000000
setcnt XR_ACCT6_UP 30 70; setcnt XR_ACCT6_DN 40 80
js="$($XR traffic)"
chk '[ "$(echo "$js" | J "d[\"wan\"][\"up\"], d[\"wan\"][\"down\"], d[\"lan\"][\"up\"], d[\"lan\"][\"down\"]")" = "5000000070 9000000080 1030 2040" ]'
chk '[ "$(echo "$js" | J "d[\"total\"][\"up\"], d[\"total\"][\"down\"], d[\"counting\"], d[\"ipv6\"]")" = "5000001100 9000002120 True True" ]'
chk '[ "$(echo "$js" | J "d[\"since\"] > 1600000000")" = True ]'

# 3. 同步不改内网列表：计数不动、不重建
: > "$MOCK/calls"; $NET sync
chk '! grep -q "^iptables -F XR_ACCT_UP" "$MOCK/calls"'
chk '[ "$($XR traffic | J "d[\"wan\"][\"up\"]")" = 5000000070 ]'

# 4. VPN 路由变化：先折算进 base 再重建，累计值不丢
echo "10.9.0.0/24" > "$RUN/guest/guest/openvpn.routes"; $NET sync
chk 'grep -qx -- "-d 10.9.0.0/24 -j RETURN" "$MOCK/chain.XR_ACCT_UP" && ! grep -q "10.8.0.0/24" "$MOCK/chain.XR_ACCT_UP"'
chk '[ ! -s "$MOCK/cnt.XR_ACCT_UP" ]'
chk '[ "$(cat "$XR_DATA/config/traffic.base")" = "5000000070 9000000080 1030 2040" ]'
setcnt XR_ACCT_UP 5 10
chk '[ "$($XR traffic | J "d[\"wan\"][\"up\"], d[\"lan\"][\"up\"]")" = "5000000080 1035" ]'

# 5. 守护进程快照：只在变化时写
. "$ROOT/module/scripts/traffic.sh"
IPT="$ROOT/tests/mock/net/iptables -w"; IP6T="$ROOT/tests/mock/net/ip6tables -w"
acct_snapshot
chk '[ "$(cat "$XR_DATA/config/traffic.snap")" = "5000000080 9000000080 1035 2040" ]'
m1="$(stat -c %Y "$XR_DATA/config/traffic.snap")"; touch -d '2000-01-01' "$XR_DATA/config/traffic.snap"
acct_snapshot
chk '[ "$(stat -c %Y "$XR_DATA/config/traffic.snap")" -lt "$m1" ]'

# 6. 停止（down）：折算保存，钩子和链全部删除；再次 up 从保存值接着累计
rmdir "$T/sysnet/xr0"; $NET down
chk '[ ! -f "$MOCK/chain.XR_ACCT" ] && [ ! -f "$MOCK/chain.XR_ACCT_UP" ] && [ ! -f "$MOCK/chain.XR_ACCT6_DN" ]'
chk '! grep -q XR_ACCT "$MOCK/hook.INPUT" "$MOCK/hook.OUTPUT" "$MOCK/hook6.INPUT" "$MOCK/hook6.OUTPUT"'
chk '[ "$(cat "$XR_DATA/config/traffic.base")" = "5000000080 9000000080 1035 2040" ]'
chk '[ "$($XR traffic | J "d[\"wan\"][\"up\"], d[\"counting\"]")" = "5000000080 False" ]'
mkdir -p "$T/sysnet/xr0"; $NET up
setcnt XR_ACCT_UP 1 2
chk '[ "$($XR traffic | J "d[\"wan\"][\"up\"], d[\"lan\"][\"up\"]")" = "5000000082 1036" ]'

# 7. 整机重启（链被清空、没来得及折算）：从快照接续
acct_snapshot
for c in XR_ACCT XR_ACCT_UP XR_ACCT_DN XR_ACCT6 XR_ACCT6_UP XR_ACCT6_DN; do rm -f "$MOCK/chain.$c" "$MOCK/cnt.$c"; done
rm -f "$MOCK"/hook*.INPUT "$MOCK"/hook*.OUTPUT
echo "1 1 1 1" > "$XR_DATA/config/traffic.base"
chk '[ "$($XR traffic | J "d[\"wan\"][\"up\"]")" = 5000000082 ]'
rm -f "$RUN/acct.sig"; $NET sync
chk '[ "$(cat "$XR_DATA/config/traffic.base")" = "5000000082 9000000080 1036 2040" ]'
chk '[ -f "$MOCK/chain.XR_ACCT_UP" ] && grep -qx -- "-i xr0 -j XR_ACCT" "$MOCK/hook.INPUT"'

# 8. 清零
setcnt XR_ACCT_UP 7 9
chk '$XR traffic-reset | grep -qx "{\"ok\":true}"'
chk '[ ! -s "$MOCK/cnt.XR_ACCT_UP" ] && [ -f "$RUN/notify.force" ]'
chk '[ "$($XR traffic | J "d[\"total\"][\"up\"], d[\"total\"][\"down\"]")" = "0 0" ]'
chk '[ "$(cat "$XR_DATA/config/traffic.snap")" = "0 0 0 0" ]'

# 9. 没有 ip6tables filter：只统计 IPv4，不报错
rmdir "$T/sysnet/xr0"; $NET down; mkdir -p "$T/sysnet/xr0"
ACCT6_FAIL=1 $NET up
chk '[ -f "$MOCK/chain.XR_ACCT_UP" ] && [ ! -f "$MOCK/chain.XR_ACCT6_UP" ]'
chk '! grep -qx -- "-j XR_ACCT6" "$MOCK/XR_FWD6"'
chk '[ "$($XR traffic | J "d[\"ipv6\"], d[\"counting\"]")" = "False True" ]'

# 10. 单位换算
chk '[ "$(acct_human 0)" = "0 B" ] && [ "$(acct_human 1023)" = "1023 B" ] && [ "$(acct_human 1536)" = "1.5 KB" ]'
chk '[ "$(acct_human 1288490189)" = "1.2 GB" ] && [ "$(acct_human 9234179686)" = "8.6 GB" ] && [ "$(acct_human 5497558138880)" = "5.0 TB" ]'

# 11. 损坏的保存文件按 0 处理
echo "garbage x" > "$XR_DATA/config/traffic.base"
chk '[ "$(acct_read_file "$XR_DATA/config/traffic.base")" = "0 0 0 0" ]'
chk '[ "$(acct_read_file "$XR_DATA/config/none")" = "0 0 0 0" ]'

# 12. 卸载脚本兜底删除统计链
chk 'grep -q "XR_ACCT XR_ACCT6" "$ROOT/module/uninstall.sh"'

[ "$fail" = 0 ] && echo "test-traffic OK"
exit "$fail"
