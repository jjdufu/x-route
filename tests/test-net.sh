#!/bin/sh
# shellcheck disable=SC2016,SC2317,SC2034
# XRouter: net.sh / vpnroutes.sh 模拟测试（假 ip / ndc / iptables，不触碰本机网络）
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
# set_kv 会整体拒绝含非法项的列表；运行时过滤是第二道防线，这里直接写文件绕过
chk '! set_kv VPN_ROUTES "10.8.0.0/24,8.8.8.0/24" && ! set_kv VPN_ROUTES "10.9.0.1/24" && set_kv VPN_ROUTES "10.8.0.0/24,192.168.10.0/24"'
sed -i 's#^VPN_ROUTES=.*#VPN_ROUTES="10.8.0.0/24,192.168.43.0/25,8.8.8.0/24,10.9.0.1/24,bad,172.29.233.0/24"#' "$CONF"

# 1. up（ndc 可用，热点暴露，蜂窝上游）
$NET up; chk '[ $? -eq 0 ]'
chk '[ "$(cat "$RUN/net.mode")" = ndc ] && [ "$(cat "$RUN/net.up_if")" = rmnet_data2 ]'
chk 'grep -q "^ip addr add 172.29.233.1/24 dev xr0" "$MOCK/calls" && grep -q "^ndc network interface add local xr0" "$MOCK/calls"'
chk 'grep -q "^ip rule add pref 4800 to 172.29.233.0/24 lookup local_network" "$MOCK/calls"'
chk 'grep -q "^ndc ipfwd enable xrouter" "$MOCK/calls" && grep -q "^ndc ipfwd add xr0 rmnet_data2" "$MOCK/calls"'
chk '[ -f "$MOCK/fwd_hook" ] && [ -f "$MOCK/nat_hook" ]'
chk 'grep -qx -- "-s 172.29.233.0/24 -o rmnet_data2 -j MASQUERADE" "$MOCK/XR_NAT"'
chk '[ "$(cat "$RUN/routes.active")" = 10.8.0.0/24 ]'
chk 'grep -q "^ndc network route add local xr0 10.8.0.0/24 172.29.233.254" "$MOCK/calls" && grep -q "^ip rule add pref 4801 to 10.8.0.0/24 lookup local_network" "$MOCK/calls"'
chk 'grep -c "格式错误\|不是私网\|主机位\|重叠" "$LOG" | grep -qx 5'
chk 'grep -q "192.168.43.0/25 与 192.168.43.1/24 重叠" "$LOG"'
# 蜂窝上游 = WAN：出站放行、回包放行、入站丢弃（与 Wi-Fi 上游规则相同）
chk 'grep -qx -- "-i xr0 -o rmnet_data2 -j ACCEPT" "$FWD" && grep -qx -- "-i rmnet_data2 -o xr0 -j DROP" "$FWD"'
chk '! grep -q -- "-i rmnet+" "$FWD"'
chk '[ "$(sed -n 1p "$FWD")" = "-o xr0 -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT" ] && [ "$(sed -n 2p "$FWD")" = "-i xr0 -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT" ]'
chk '[ "$(sed -n 3p "$FWD")" = "-i xr0 -o rmnet_data2 -j ACCEPT" ]'
chk 'grep -qx -- "-i wlan1 -o xr0 -d 172.29.233.0/24 -j ACCEPT" "$FWD" && grep -qx -- "-i wlan1 -o xr0 -d 10.8.0.0/24 -j ACCEPT" "$FWD"'
chk '[ "$(tail -2 "$FWD" | tr "\n" "|")" = "-o xr0 -j DROP|-i xr0 -j DROP|" ]'
chk '$NET status | grep -q "\"mode\":\"ndc\",\"up_if\":\"rmnet_data2\",\"expose\":\"hotspot\",\"expose_if\":\[\"wlan1\"\]"'
# 默认全局，但 VM 未就绪：不得安装接管规则，以免把手机上网搞断
chk '! grep -q "rule add pref 19100" "$MOCK/calls" && [ ! -f "$RUN/net.gw" ]'

# 2. 上游切到 Wi-Fi，XR_EXPOSE=all -> Wi-Fi 也暴露；蜂窝永不暴露
set_kv XR_EXPOSE all
echo "11000:	from all fwmark 0x0/0xffff iif lo lookup wlan0" > "$MOCK/iprule"
mkdir -p "$T/sysnet/xr0"; : > "$MOCK/calls"; $NET sync
chk 'grep -q "^ndc ipfwd remove xr0 rmnet_data2" "$MOCK/calls" && grep -q "^ndc ipfwd add xr0 wlan0" "$MOCK/calls"'
chk '[ "$(cat "$RUN/net.expose_if")" = "wlan0 wlan1 " ]'
chk 'grep -qx -- "-i wlan0 -o xr0 -d 10.8.0.0/24 -j ACCEPT" "$FWD"'
chk 'grep -qx -- "-i xr0 -o wlan0 -j ACCEPT" "$FWD" && grep -qx -- "-s 172.29.233.0/24 -o wlan0 -j MASQUERADE" "$MOCK/XR_NAT"'
chk '! grep -qx -- "-i wlan0 -o xr0 -j DROP" "$FWD"'   # all 明确放行了 Wi-Fi 上游局域网
set_kv XR_EXPOSE hotspot; $NET sync
chk 'grep -qx -- "-i wlan0 -o xr0 -j DROP" "$FWD" && ! grep -q -- "-i wlan0 -o xr0 -d" "$FWD"'   # Wi-Fi 上游同样默认不信任
set_kv XR_EXPOSE all
# 纯蜂窝 + 464xlat：IPv4 实际出口是 v4-rmnet_data2，出站/NAT/ipfwd 都要覆盖
echo "default dev v4-rmnet_data2 proto static scope link" > "$MOCK/rt.rmnet_data2"
echo "11000:	from all fwmark 0x0/0xffff iif lo lookup rmnet_data2" > "$MOCK/iprule"; : > "$MOCK/calls"; $NET sync
chk '[ "$(cat "$RUN/net.expose_if")" = "wlan1 " ] && ! grep -q -- "-i rmnet_data2 -o xr0 .*ACCEPT" "$FWD"'
chk '[ "$(cat "$RUN/net.up_out")" = "rmnet_data2 v4-rmnet_data2 " ]'
chk 'grep -q "^ndc ipfwd add xr0 rmnet_data2" "$MOCK/calls" && grep -q "^ndc ipfwd add xr0 v4-rmnet_data2" "$MOCK/calls" && grep -q "^ndc ipfwd remove xr0 wlan0" "$MOCK/calls"'
chk 'grep -qx -- "-s 172.29.233.0/24 -o v4-rmnet_data2 -j MASQUERADE" "$MOCK/XR_NAT" && grep -qx -- "-s 172.29.233.0/24 -o rmnet_data2 -j MASQUERADE" "$MOCK/XR_NAT"'
chk 'grep -qx -- "-i xr0 -o v4-rmnet_data2 -j ACCEPT" "$FWD" && grep -qx -- "-i v4-rmnet_data2 -o xr0 -j DROP" "$FWD" && grep -qx -- "-i rmnet_data2 -o xr0 -j DROP" "$FWD"'
# 464xlat 出口出现/消失（上游名不变）也要重建
rm -f "$MOCK/rt.rmnet_data2"; $NET sync
chk '[ "$(cat "$RUN/net.up_out")" = "rmnet_data2 " ] && ! grep -q v4-rmnet "$FWD"'
# 手机侧不改写 VM 的包：不用 mangle、不限协议/端口、不打 MARK/DSCP/MSS；NAT 只按源网段
chk '! grep -qE "^iptables .*(-t mangle|--dport|--sport|-p udp|-p tcp|MARK|DSCP|TCPMSS|TOS)" "$MOCK/calls"'
chk '[ -z "$(grep -v -- "^-s 172.29.233.0/24 -o [a-z0-9_-]* -j MASQUERADE$" "$MOCK/XR_NAT")" ]'
# 3. 去掉 VPN 网段 -> 路由与规则撤销
set_kv XR_EXPOSE hotspot; set_kv VPN_ROUTES ""; : > "$MOCK/calls"; $NET sync
chk 'grep -q "^ndc network route remove local xr0 10.8.0.0/24 172.29.233.254" "$MOCK/calls" && grep -q "^ip rule del pref 4801 to 10.8.0.0/24" "$MOCK/calls"'
chk '[ -z "$(cat "$RUN/routes.active")" ] && ! grep -q 10.8.0.0 "$FWD"'
XR_EXPOSE=off; set_kv XR_EXPOSE off; $NET sync
chk '! grep -q -- "-i wlan1" "$FWD" && [ "$(cat "$RUN/net.expose_if")" = " " -o -z "$(tr -d " " < "$RUN/net.expose_if")" ]'
# 4. down
rmdir "$T/sysnet/xr0"; : > "$MOCK/calls"; $NET down
chk '[ ! -f "$MOCK/fwd_hook" ] && [ ! -f "$MOCK/nat_hook" ] && [ ! -f "$MOCK/XR_FWD" ] && [ ! -f "$MOCK/XR_NAT" ]'
chk 'grep -q "^ip link del xr0" "$MOCK/calls" && grep -q "^ndc ipfwd disable xrouter" "$MOCK/calls" && grep -q "^ndc network interface remove local xr0" "$MOCK/calls"'
chk '! ls "$RUN"/net.* >/dev/null 2>&1 && [ ! -f "$RUN/net_watch.pid" ]'

# 5. ndc 不可用 -> table 97 回退，ip_forward 改写并在 down 时恢复
set_kv VPN_ROUTES "10.8.0.0/24"; : > "$MOCK/calls"
NDC_FAIL=1 $NET up
chk '[ "$(cat "$RUN/net.mode")" = fallback ] && [ "$(cat "$T/proc/sys/net/ipv4/ip_forward")" = 1 ]'
chk 'grep -q "^ip route replace 172.29.233.0/24 dev xr0 table 97" "$MOCK/calls" && grep -q "^ip rule add pref 4800 to 172.29.233.0/24 lookup 97" "$MOCK/calls"'
chk 'grep -q "^ip rule add pref 18900 iif xr0 lookup rmnet_data2" "$MOCK/calls"'
chk 'grep -q "^ip route replace 10.8.0.0/24 via 172.29.233.254 dev xr0 table 97" "$MOCK/calls" && grep -q "^ip rule add pref 4801 to 10.8.0.0/24 lookup 97" "$MOCK/calls"'
NDC_FAIL=1 $NET down
chk '[ "$(cat "$T/proc/sys/net/ipv4/ip_forward")" = 0 ] && grep -q "^ip route del 10.8.0.0/24 table 97" "$MOCK/calls"'

# 6. 运行模式：全局且就绪 -> 表 98 + pref 19100 + 热点进 xr0；旁路或未就绪则撤销
echo "default via 10.0.0.1 dev rmnet_data2" > "$MOCK/devrt.rmnet_data2"
echo "192.168.43.0/24 dev wlan1 proto kernel scope link src 192.168.43.1" > "$MOCK/devrt.wlan1"
set_kv XR_MODE global
echo ready > "$RUN/state"
echo "$$" > "$RUN/crosvm.pid"
: > "$MOCK/calls"
# 保证 xr0 存在（上次 down 已删）
mkdir -p "$T/sysnet/xr0"
# 上一段 down 清掉了链，需要重新 up
$NET up
chk 'grep -q "ip rule add pref 19100 lookup 98" "$MOCK/calls"'
chk 'grep -q "ip route replace default via 172.29.233.254 dev xr0 src 172.29.233.1 table 98" "$MOCK/calls"'
chk 'grep -q "192.168.43.0/24 dev wlan1" "$MOCK/calls" && grep -q "table 98" "$MOCK/calls"'
chk '[ "$(cat "$RUN/net.gw")" = 1 ]'
chk 'grep -qx -- "-i wlan1 -o xr0 -j ACCEPT" "$FWD" && grep -qx -- "-i xr0 -o wlan1 -j ACCEPT" "$FWD"'
chk 'grep -qx -- "-i wlan1 -o rmnet_data2 -j DROP" "$FWD"'
# 未就绪立即撤销
rm -f "$RUN/state" "$RUN/crosvm.pid"; : > "$MOCK/calls"; $NET sync
chk 'grep -q "ip rule del pref 19100" "$MOCK/calls" && [ ! -f "$RUN/net.gw" ]'
chk '! grep -qx -- "-i wlan1 -o rmnet_data2 -j DROP" "$FWD"'
# 旁路即使就绪也不接管
echo ready > "$RUN/state"; echo "$$" > "$RUN/crosvm.pid"
set_kv XR_MODE bypass; : > "$MOCK/calls"; $NET sync
chk '! grep -q "pref 19100 lookup 98" "$MOCK/calls" && [ ! -f "$RUN/net.gw" ]'
# 仍不改写 VM 出站包
chk '! grep -qE "^iptables .*(-t mangle|--dport|--sport|-p udp|-p tcp|MARK|DSCP|TCPMSS|TOS)" "$MOCK/calls"'
$NET down

if [ "$fail" = 0 ]; then echo "test-net OK"; else echo "test-net FAILED"; exit 1; fi
