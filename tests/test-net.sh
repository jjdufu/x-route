#!/bin/sh
# shellcheck disable=SC2016,SC2317,SC2034
# x-route: net.sh / vpnroutes.sh 模拟测试（假 ip / ndc / iptables，不触碰本机网络）
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
T="$(mktemp -d)"; MOCK="$T/mock"; mkdir -p "$MOCK" "$T/proc/sys/net/ipv4/conf/xr0" "$T/proc/sys/net/ipv6/conf/xr0"
fail=0; chk() { if eval "$1"; then :; else echo "FAIL: $1"; fail=1; fi; }
trap 'rm -rf "$T"' EXIT
echo 0 > "$T/proc/sys/net/ipv4/ip_forward"
export PATH="$ROOT/tests/mock/net:$PATH" MOCK XR_DATA="$T/data" MODDIR="$ROOT/module" XR_SYSBIN="$ROOT/tests/mock/net" XR_PROC="$T/proc" XR_SYSNET="$T/sysnet"
. "$ROOT/module/scripts/common.sh"; ensure_dirs
cp "$ROOT/module/config/config.conf" "$CONF"
NET="sh $ROOT/module/scripts/net.sh"
FWD="$MOCK/XR_FWD"
echo "11000:	from all fwmark 0x0/0xffff iif lo lookup rmnet_data2" > "$MOCK/iprule"
echo "192.168.43.0/24 dev wlan1 proto static scope link" > "$MOCK/lnroutes"
echo "30: wlan1    inet 192.168.43.1/24 brd 192.168.43.255 scope global wlan1" > "$MOCK/addr.wlan1"

# 1. up（ndc 可用，热点暴露，蜂窝上游）
$NET up; chk '[ $? -eq 0 ]'
chk '[ "$(cat "$RUN/net.mode")" = ndc ] && [ "$(cat "$RUN/net.up_if")" = rmnet_data2 ]'
chk 'grep -q "^ip addr add 172.29.233.1/24 dev xr0" "$MOCK/calls" && grep -q "^ndc network interface add local xr0" "$MOCK/calls"'
chk 'grep -q "^ip rule add pref 4800 to 172.29.233.0/24 lookup local_network" "$MOCK/calls"'
chk 'grep -q "^ndc ipfwd enable xroute" "$MOCK/calls" && grep -q "^ndc ipfwd add xr0 rmnet_data2" "$MOCK/calls"'
chk '[ -f "$MOCK/fwd_hook" ] && [ -f "$MOCK/nat_hook" ]'
chk 'grep -qx -- "-s 172.29.233.0/24 -o rmnet_data2 -j MASQUERADE" "$MOCK/XR_NAT"'
chk 'grep -qx -- "-i rmnet+ -o xr0 -j DROP" "$FWD" && grep -qx -- "-i v4-ccmni+ -o xr0 -j DROP" "$FWD"'
chk 'grep -qx -- "-i xr0 -o rmnet_data2 -j ACCEPT" "$FWD"'
chk '[ "$(tail -1 "$FWD")" = "-i xr0 ! -o rmnet_data2 -j DROP" ]'
chk '[ "$(head -1 "$FWD")" = "-i rmnet+ -o xr0 -j DROP" ]'
chk '$NET status | grep -q "\"mode\":\"ndc\",\"up_if\":\"rmnet_data2\",\"expose\":\"hotspot\",\"expose_if\":\[\"wlan1\"\]"'

# 2. 上游切到 Wi-Fi，XR_EXPOSE=all -> Wi-Fi 也暴露；蜂窝永不暴露
set_kv XR_EXPOSE all
echo "11000:	from all fwmark 0x0/0xffff iif lo lookup wlan0" > "$MOCK/iprule"
mkdir -p "$T/sysnet/xr0"; : > "$MOCK/calls"; $NET sync
chk 'grep -q "^ndc ipfwd remove xr0 rmnet_data2" "$MOCK/calls" && grep -q "^ndc ipfwd add xr0 wlan0" "$MOCK/calls"'
chk '[ "$(cat "$RUN/net.expose_if")" = "wlan0 wlan1 " ]'
echo "11000:	from all fwmark 0x0/0xffff iif lo lookup rmnet_data2" > "$MOCK/iprule"; $NET sync
chk '[ "$(cat "$RUN/net.expose_if")" = "wlan1 " ] && ! grep -q -- "-i rmnet_data2 -o xr0 .*ACCEPT" "$FWD"'
# 3. 去掉 VPN 网段 -> 路由与规则撤销
XR_EXPOSE=off; set_kv XR_EXPOSE off; $NET sync
chk '! grep -q -- "-i wlan1" "$FWD" && [ "$(cat "$RUN/net.expose_if")" = " " -o -z "$(tr -d " " < "$RUN/net.expose_if")" ]'
# 4. down
rmdir "$T/sysnet/xr0"; : > "$MOCK/calls"; $NET down
chk '[ ! -f "$MOCK/fwd_hook" ] && [ ! -f "$MOCK/nat_hook" ] && [ ! -f "$MOCK/XR_FWD" ] && [ ! -f "$MOCK/XR_NAT" ]'
chk 'grep -q "^ip link del xr0" "$MOCK/calls" && grep -q "^ndc ipfwd disable xroute" "$MOCK/calls" && grep -q "^ndc network interface remove local xr0" "$MOCK/calls"'
chk '! ls "$RUN"/net.* >/dev/null 2>&1 && [ ! -f "$RUN/net_watch.pid" ]'

# 5. ndc 不可用 -> table 97 回退，ip_forward 改写并在 down 时恢复
NDC_FAIL=1 $NET up
chk '[ "$(cat "$RUN/net.mode")" = fallback ] && [ "$(cat "$T/proc/sys/net/ipv4/ip_forward")" = 1 ]'
chk 'grep -q "^ip route replace 172.29.233.0/24 dev xr0 table 97" "$MOCK/calls" && grep -q "^ip rule add pref 4800 to 172.29.233.0/24 lookup 97" "$MOCK/calls"'
chk 'grep -q "^ip rule add pref 18900 iif xr0 lookup rmnet_data2" "$MOCK/calls"'
NDC_FAIL=1 $NET down

if [ "$fail" = 0 ]; then echo "test-net OK"; else echo "test-net FAILED"; exit 1; fi
