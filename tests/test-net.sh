#!/bin/sh
# shellcheck disable=SC2016,SC2317,SC2034
# XRouter: net.sh / vpnroutes.sh 模拟测试（假 ip / ndc / iptables，不触碰本机网络）
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
T="$(mktemp -d)"; MOCK="$T/mock"; mkdir -p "$MOCK" "$T/proc/sys/net/ipv4/conf/xr0" "$T/proc/sys/net/ipv6/conf/xr0" "$T/proc/4321"
printf 'crosvm\0' > "$T/proc/4321/cmdline"
echo '4321 (crosvm) S 1 1 1 0 -1 4194304 0 0 0 0 0 0 0 0 20 0 1 0 1 0 0 18446744073709551615 0 0 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0' > "$T/proc/4321/stat"
fail=0; chk() { if eval "$1"; then :; else echo "FAIL: $1"; fail=1; fi; }
trap 'rm -rf "$T"' EXIT
echo 0 > "$T/proc/sys/net/ipv4/ip_forward"
export PATH="$ROOT/tests/mock/net:$PATH" MOCK XR_DATA="$T/data" MODDIR="$ROOT/module" XR_SYSBIN="$ROOT/tests/mock/net" XR_PROC="$T/proc" XR_PROC_BASE="$T/proc" XR_SYSNET="$T/sysnet"
CONF="$XR_DATA/config/config.conf"; RUN="$XR_DATA/run"; LOG="$XR_DATA/log/module.log"; mkdir -p "$XR_DATA/config" "$RUN" "$XR_DATA/log"
cp "$ROOT/module/config/config.conf" "$CONF"
NET="sh $ROOT/module/scripts/net.sh"
set_kv() { sh "$ROOT/module/scripts/xr" internal set-kv "$1" "$2" >/dev/null; }
FWD="$MOCK/XR_FWD"
echo "11000:	from all fwmark 0x0/0xffff iif lo lookup rmnet_data2" > "$MOCK/iprule"
echo "192.168.43.0/24 dev wlan1 proto static scope link" > "$MOCK/lnroutes"
echo "30: wlan1    inet 192.168.43.1/24 brd 192.168.43.255 scope global wlan1" > "$MOCK/addr.wlan1"
# 客侧上报的实际路由（含非法项，由运行时过滤）；默认路由应被忽略
mkdir -p "$RUN/guest/guest"
printf '%s\n' "10.8.0.0/24" "192.168.43.0/25" "8.8.8.0/24" "10.9.0.1/24" "bad" "172.29.233.0/24" "0.0.0.0/0" "0.0.0.0/1" "128.0.0.0/1" \
  "10.58.88.2" "10.1.2.3/32" "10.8.0.0/255.255.255.0" "2001:db8:10::/64" "2001:db8::5" "::/0" "::/1" "8000::/1" \
  "not-a-route" "gggg::1" "10.0.0.1/99" "fd7a:7872:e9::/64" "2001:db8:10::1/64" > "$RUN/guest/guest/openvpn.routes"

# 1. up（ndc 可用，热点暴露，蜂窝上游）
$NET up; chk '[ $? -eq 0 ]'
chk '[ "$(cat "$RUN/net.mode")" = ndc ] && [ "$(cat "$RUN/net.up_if")" = rmnet_data2 ]'
chk 'grep -q "^ip addr add 172.29.233.1/24 dev xr0" "$MOCK/calls" && grep -q "^ndc network interface add local xr0" "$MOCK/calls"'
chk 'grep -q "^ip rule add pref 4800 to 172.29.233.0/24 lookup local_network" "$MOCK/calls"'
chk 'grep -q "^ndc ipfwd enable xrouter" "$MOCK/calls" && grep -q "^ndc ipfwd add xr0 rmnet_data2" "$MOCK/calls"'
chk '[ -f "$MOCK/fwd_hook" ] && [ -f "$MOCK/nat_hook" ]'
chk 'grep -qx -- "-s 172.29.233.0/24 -o rmnet_data2 -j MASQUERADE" "$MOCK/XR_NAT"'
chk 'grep -q "10.8.0.0/24" "$RUN/routes.active" && grep -q "8.8.8.0/24" "$RUN/routes.active" && ! grep -q "0.0.0.0" "$RUN/routes.active"'
chk 'grep -qx "10.58.88.2/32" "$RUN/routes.active" && grep -qx "10.1.2.3/32" "$RUN/routes.active"'
chk 'grep -qx "2001:db8:10::/64" "$RUN/routes.active" && grep -qx "2001:db8::5/128" "$RUN/routes.active"'
chk '! grep -q "fd7a:7872:e9" "$RUN/routes.active" && ! grep -q "::/0" "$RUN/routes.active"'
chk 'grep -q "^ndc network route add local xr0 10.8.0.0/24 172.29.233.254" "$MOCK/calls" && grep -q "^ip rule add pref 4801 to 10.8.0.0/24 lookup local_network" "$MOCK/calls"'
chk 'grep -q "^ndc network route add local xr0 8.8.8.0/24 172.29.233.254" "$MOCK/calls"'
chk 'grep -q "^ndc network route add local xr0 10.58.88.2/32 172.29.233.254" "$MOCK/calls" && grep -q "^ip rule add pref 4801 to 10.58.88.2/32 lookup local_network" "$MOCK/calls"'
chk 'grep -q "^ndc network route add local xr0 2001:db8:10::/64 fd7a:7872:e9::254" "$MOCK/calls"'
chk 'grep -q "^ip -6 rule add pref 4801 to 2001:db8:10::/64 lookup local_network" "$MOCK/calls"'
chk 'grep -q "^ndc network route add local xr0 2001:db8::5/128 fd7a:7872:e9::254" "$MOCK/calls"'
chk 'grep -c "格式错误\|主机位\|重叠" "$LOG" | grep -qx 9'
chk 'grep -q "192.168.43.0/25 与 192.168.43.1/24 重叠" "$LOG"'
# 蜂窝上游 = WAN：出站放行、回包放行、入站丢弃（与 Wi-Fi 上游规则相同）
chk 'grep -qx -- "-i xr0 -o rmnet_data2 -j ACCEPT" "$FWD" && grep -qx -- "-i rmnet_data2 -o xr0 -j DROP" "$FWD"'
chk '! grep -q -- "-i rmnet+" "$FWD"'
# 第一条是流量统计（只计数后 RETURN），其后才是回包放行
chk '[ "$(sed -n 1p "$FWD")" = "-j XR_ACCT" ]'
chk '[ "$(sed -n 2p "$FWD")" = "-o xr0 -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT" ] && [ "$(sed -n 3p "$FWD")" = "-i xr0 -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT" ]'
chk '[ "$(sed -n 4p "$FWD")" = "-i xr0 -o rmnet_data2 -j ACCEPT" ]'
chk 'grep -qx -- "-i wlan1 -o xr0 -d 172.29.233.0/24 -j ACCEPT" "$FWD" && grep -qx -- "-i wlan1 -o xr0 -d 10.8.0.0/24 -j ACCEPT" "$FWD" && grep -qx -- "-i wlan1 -o xr0 -d 8.8.8.0/24 -j ACCEPT" "$FWD"'
chk 'grep -qx -- "-i wlan1 -o xr0 -d 10.58.88.2/32 -j ACCEPT" "$FWD" && grep -qx -- "-i wlan1 -o xr0 -d 10.1.2.3/32 -j ACCEPT" "$FWD"'
chk 'grep -qx -- "-i wlan1 -o xr0 -d 2001:db8:10::/64 -j ACCEPT" "$MOCK/XR_FWD6" && grep -qx -- "-i wlan1 -o xr0 -d 2001:db8::5/128 -j ACCEPT" "$MOCK/XR_FWD6"'
chk '[ "$(tail -2 "$FWD" | tr "\n" "|")" = "-o xr0 -j DROP|-i xr0 -j DROP|" ]'
chk '$NET status | grep -q "\"mode\":\"ndc\",\"up_if\":\"rmnet_data2\",\"expose\":\"lan\",\"expose_if\":\[\"wlan1\"\]"'
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
set_kv XR_EXPOSE lan; $NET sync
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
# 手机侧不改写 VM 的包：不限协议/端口、不打 MARK/DSCP/TOS；NAT 只按源网段。
# 唯一例外是 XR_MSS 链里按路径 MTU 钳制 SYN 的 MSS（不改载荷，只防大包被上游丢弃）
chk '! grep -v "XR_MSS" "$MOCK/calls" | grep -qE "^iptables .*(-t mangle|--dport|--sport|-p udp|-p tcp|MARK|DSCP|TCPMSS|TOS)"'
chk '[ -z "$(grep -v -- "^-s 172.29.233.0/24 -o [a-z0-9_-]* -j MASQUERADE$" "$MOCK/XR_NAT")" ]'
# 3. 固定热点网段重叠路由被拒绝；去掉 VPN 网段 -> 路由与规则撤销
echo "172.28.222.254/24" > "$RUN/net.hs_gw"
echo "172.28.222.0/24" >> "$RUN/guest/guest/openvpn.routes"; $NET sync
chk '! grep -q "172.28.222.0/24" "$RUN/routes.active" && grep -q "172.28.222.0/24 与 172.28.222.254/24 重叠" "$LOG"'
rm -f "$RUN/net.hs_gw"
set_kv XR_EXPOSE lan; : > "$RUN/guest/guest/openvpn.routes"; : > "$MOCK/calls"; $NET sync
chk 'grep -q "^ndc network route remove local xr0 10.8.0.0/24 172.29.233.254" "$MOCK/calls" && grep -q "^ip rule del pref 4801 to 10.8.0.0/24" "$MOCK/calls"'
chk '[ -z "$(cat "$RUN/routes.active")" ] && ! grep -q 10.8.0.0 "$FWD"'
XR_EXPOSE=local; set_kv XR_EXPOSE local; $NET sync
chk '! grep -q -- "-i wlan1" "$FWD" && [ "$(cat "$RUN/net.expose_if")" = " " -o -z "$(tr -d " " < "$RUN/net.expose_if")" ]'
# 4. down
rmdir "$T/sysnet/xr0"; : > "$MOCK/calls"; $NET down
chk '[ ! -f "$MOCK/fwd_hook" ] && [ ! -f "$MOCK/nat_hook" ] && [ ! -f "$MOCK/XR_FWD" ] && [ ! -f "$MOCK/XR_NAT" ]'
chk 'grep -q "^ip link del xr0" "$MOCK/calls" && grep -q "^ndc ipfwd disable xrouter" "$MOCK/calls" && grep -q "^ndc network interface remove local xr0" "$MOCK/calls"'
chk '! ls "$RUN"/net.* >/dev/null 2>&1 && [ ! -f "$RUN/net_watch.pid" ]'

# 5. ndc 不可用 -> table 97 回退，ip_forward 改写并在 down 时恢复
echo "10.8.0.0/24" > "$RUN/guest/guest/openvpn.routes"; : > "$MOCK/calls"
NDC_FAIL=1 $NET up
chk '[ "$(cat "$RUN/net.mode")" = fallback ] && [ "$(cat "$T/proc/sys/net/ipv4/ip_forward")" = 1 ]'
chk 'grep -q "^ip route replace 172.29.233.0/24 dev xr0 table 97" "$MOCK/calls" && grep -q "^ip rule add pref 4800 to 172.29.233.0/24 lookup 97" "$MOCK/calls"'
chk 'grep -q "^ip rule add pref 18900 iif xr0 lookup rmnet_data2" "$MOCK/calls"'
chk 'grep -q "^ip route replace 10.8.0.0/24 via 172.29.233.254 dev xr0 table 97" "$MOCK/calls" && grep -q "^ip rule add pref 4801 to 10.8.0.0/24 lookup 97" "$MOCK/calls"'
NDC_FAIL=1 $NET down
chk '[ "$(cat "$T/proc/sys/net/ipv4/ip_forward")" = 0 ] && grep -q "^ip route del 10.8.0.0/24 table 97" "$MOCK/calls"'

# 6. 运行模式：全局且就绪 -> 表 233 + pref 19100 + 热点进 xr0；旁路或未就绪则撤销
echo "default via 10.0.0.1" > "$MOCK/devrt.rmnet_data2"
echo "192.168.43.0/24 proto kernel scope link src 192.168.43.1" > "$MOCK/devrt.wlan1"
set_kv XR_MODE global
echo ready > "$RUN/state"
echo 4321 > "$RUN/crosvm.pid"
: > "$MOCK/calls"
# 保证 xr0 存在（上次 down 已删）
mkdir -p "$T/sysnet/xr0"
# 上一段 down 清掉了链，需要重新 up
$NET up
chk 'grep -q "ip rule add pref 19100 lookup 233" "$MOCK/calls"'
chk 'grep -q "^ip rule add pref 18800 iif xr0 lookup local_network" "$MOCK/calls"'
chk 'grep -q "^ip rule add pref 18900 iif xr0 lookup rmnet_data2" "$MOCK/calls"'
chk 'grep -q "ip route replace default via 172.29.233.254 dev xr0 src 172.29.233.1 table 233" "$MOCK/calls"'
chk 'grep -q "192.168.43.0/24 proto kernel scope link src 192.168.43.1 dev wlan1 table 233" "$MOCK/calls"'
chk '[ "$(cat "$RUN/net.gw")" = block ]'
# 虚拟机 IPv6 未开：本机 / 热点 IPv6 被拒绝（不绕过虚拟机），应用立即回落 IPv4
chk 'grep -q "^ip -6 route replace unreachable default table 233" "$MOCK/calls" && grep -q "^ip -6 rule add pref 19100 lookup 233" "$MOCK/calls"'
chk 'grep -qx -- "-i wlan1 -o rmnet_data2 -j REJECT --reject-with icmp6-adm-prohibited" "$MOCK/XR_FWD6"'
chk '! grep -qx -- "-i xr0 -o rmnet_data2 -j ACCEPT" "$MOCK/XR_FWD6"'
chk 'grep -qx -- "-i wlan1 -o xr0 -j ACCEPT" "$FWD" && grep -qx -- "-i xr0 -o wlan1 -j ACCEPT" "$FWD"'
chk 'grep -qx -- "-i wlan1 -o rmnet_data2 -j DROP" "$FWD"'
# 未就绪立即撤销
rm -f "$RUN/state" "$RUN/crosvm.pid"; : > "$MOCK/calls"; $NET sync
chk 'grep -q "ip rule del pref 19100" "$MOCK/calls" && [ ! -f "$RUN/net.gw" ]'
chk '! grep -qx -- "-i wlan1 -o rmnet_data2 -j DROP" "$FWD"'
# 旁路即使就绪也不接管
echo ready > "$RUN/state"; echo 4321 > "$RUN/crosvm.pid"
set_kv XR_MODE bypass; : > "$MOCK/calls"; $NET sync
chk '! grep -q "pref 19100 lookup 233" "$MOCK/calls" && [ ! -f "$RUN/net.gw" ]'
# 仍不改写 VM 出站包（XR_MSS 的 MSS 钳制除外）
chk '! grep -v "XR_MSS" "$MOCK/calls" | grep -qE "^iptables .*(-t mangle|--dport|--sport|-p udp|-p tcp|MARK|DSCP|TCPMSS|TOS)"'
chk '! grep -q "^ip -6 rule add pref 19100" "$MOCK/calls" && ! grep -q -- "REJECT" "$MOCK/XR_FWD6"'
$NET down

# 7. IPv6 经邻居代理出网：客侧打开后从上游前缀取地址，不依赖 ip6tables nat；关掉后恢复拒绝
set_kv XR_MODE global; set_kv XR_EXPOSE lan
echo "default via fe80::1 dev rmnet_data2 proto ra metric 1024 pref medium" > "$MOCK/rt6.rmnet_data2"
echo "2409:8a00:1:2::/64 proto kernel metric 256 expires 7000sec pref medium" > "$MOCK/devrt6.main.rmnet_data2"
echo "2409:8a00:1:3::/64 proto static metric 1024 pref medium" > "$MOCK/devrt6.local_network.wlan1"
printf '%s\n' "2: rmnet_data2    inet6 fe80::abcd/64 scope link" "2: rmnet_data2    inet6 2409:8a00:1:2:1111:2222:3333:4444/64 scope global" > "$MOCK/addr6.rmnet_data2"
mkdir -p "$T/sysnet/xr0"; echo ready > "$RUN/state"; echo 4321 > "$RUN/crosvm.pid"; echo 1 > "$RUN/guest/guest/ipv6"
: > "$MOCK/calls"; $NET up
WAN="2409:8a00:0001:0002:5054:00ff:fe1d:e9fe"
chk 'grep -q "^ip -6 addr add fd7a:7872:e9::1/64 dev xr0 nodad" "$MOCK/calls" && [ "$(cat "$T/proc/sys/net/ipv6/conf/xr0/disable_ipv6")" = 0 ]'
chk 'grep -q "^ndc network route add local xr0 fd7a:7872:e9::/64" "$MOCK/calls" && grep -q "^ip -6 rule add pref 4800 to fd7a:7872:e9::/64 lookup local_network" "$MOCK/calls"'
chk 'grep -q "^ip -6 rule add pref 18800 iif xr0 lookup local_network" "$MOCK/calls" && grep -q "^ip -6 rule add pref 18900 iif xr0 lookup rmnet_data2" "$MOCK/calls"'
chk 'grep -q "^ip -6 route replace default via fd7a:7872:e9::254 dev xr0 src fd7a:7872:e9::1 table 233" "$MOCK/calls" && grep -q "^ip -6 rule add pref 19100 lookup 233" "$MOCK/calls"'
chk 'grep -q "^ip -6 route replace 2409:8a00:1:2::/64 proto kernel metric 256 pref medium dev rmnet_data2 table 233" "$MOCK/calls"'
chk 'grep -q "^ip -6 route replace 2409:8a00:1:3::/64 proto static metric 1024 pref medium dev wlan1 table 233" "$MOCK/calls"'
chk '[ "$(cat "$RUN/net.gw")" = v6 ] && [ -f "$MOCK/fwd6_hook" ] && [ -f "$RUN/net.v6proxy" ] && [ ! -f "$RUN/net.nat6" ]'
chk '[ "$(cat "$T/proc/sys/net/ipv6/conf/rmnet_data2/proxy_ndp")" = 1 ] && [ "$(cat "$T/proc/sys/net/ipv6/conf/all/forwarding")" = 1 ]'
chk 'grep -q "^ip -6 neigh replace proxy $WAN dev rmnet_data2" "$MOCK/calls"'
chk 'grep -q "^ip -6 route replace $WAN/128 dev xr0" "$MOCK/calls"'
chk '[ "$(cat "$RUN/guest/host/ipv6.wan")" = "$WAN/64" ] && grep -q "IPv6 邻居代理" "$LOG"'
chk '! grep -qE "^ip6tables .*?-t nat -[AN]" "$MOCK/calls" && [ ! -f "$MOCK/XR_NAT6" -o ! -s "$MOCK/XR_NAT6" ]'
chk 'grep -qx -- "-i xr0 -o rmnet_data2 -j ACCEPT" "$MOCK/XR_FWD6" && grep -qx -- "-i wlan1 -o xr0 -j ACCEPT" "$MOCK/XR_FWD6" && grep -qx -- "-i xr0 -o wlan1 -j ACCEPT" "$MOCK/XR_FWD6"'
chk 'grep -qx -- "-i wlan1 -o xr0 -d fd7a:7872:e9::/64 -j ACCEPT" "$MOCK/XR_FWD6" && grep -qx -- "-i rmnet_data2 -o xr0 -j DROP" "$MOCK/XR_FWD6"'
chk '[ "$(tail -2 "$MOCK/XR_FWD6" | tr "\n" "|")" = "-o xr0 -j DROP|-i xr0 -j DROP|" ]'
chk '$NET status | grep -q "\"gw\":\"v6\""'
# 客侧关闭 IPv6（不重启虚拟机）-> 拆除代理并拒绝旁路
echo 0 > "$RUN/guest/guest/ipv6"; : > "$MOCK/calls"; $NET sync
chk '[ "$(cat "$RUN/net.gw")" = block ] && [ ! -f "$RUN/net.v6proxy" ] && grep -q "^ip -6 route replace unreachable default table 233" "$MOCK/calls"'
chk '! grep -q "default via fd7a:7872:e9::254" "$MOCK/calls" && ! grep -qx -- "-i wlan1 -o xr0 -j ACCEPT" "$MOCK/XR_FWD6"'
chk 'grep -q "^ip -6 neigh del proxy $WAN dev rmnet_data2" "$MOCK/calls" && [ "$(cat "$T/proc/sys/net/ipv6/conf/rmnet_data2/proxy_ndp")" = 0 ]'
chk '[ ! -s "$RUN/guest/host/ipv6.wan" ]'
# 内核无 ip6 nat 时，只要有可共享前缀，仍走邻居代理
$NET down; echo 1 > "$RUN/guest/guest/ipv6"; mkdir -p "$T/sysnet/xr0"; : > "$MOCK/calls"
NAT6_FAIL=1 $NET up
chk '[ "$(cat "$RUN/net.gw")" = v6 ] && [ -f "$RUN/net.v6proxy" ] && [ ! -f "$RUN/net.nat6" ]'
chk '! grep -q "ip6tables nat 不可用" "$LOG" && grep -q "^ip -6 neigh replace proxy $WAN dev rmnet_data2" "$MOCK/calls"'
# 上游只有 /128：无法共享前缀，全局下只拦截
$NET down; printf '%s\n' "2: rmnet_data2    inet6 2409:8a00:1:2:1111:2222:3333:4444/128 scope global" > "$MOCK/addr6.rmnet_data2"
mkdir -p "$T/sysnet/xr0"; echo 1 > "$RUN/guest/guest/ipv6"; echo ready > "$RUN/state"; echo 4321 > "$RUN/crosvm.pid"
: > "$MOCK/calls"; $NET up
chk '[ "$(cat "$RUN/net.gw")" = block ] && [ ! -f "$RUN/net.v6proxy" ] && grep -q "没有可共享的 IPv6 前缀" "$LOG"'
chk 'grep -q "^ip -6 route replace unreachable default table 233" "$MOCK/calls"'
# 拦截时下游 IPv6 立即拒绝：导向表 235，再由 XR_FWD6 回 TCP RST / 管理禁止（不靠限速的 unreachable）
chk 'grep -q "^ip -6 rule add pref 19060 iif wlan1 lookup 235" "$MOCK/calls" && grep -q "^ip -6 route replace default dev xr0 table 235" "$MOCK/calls"'
chk 'grep -qx -- "-i wlan1 -o xr0 -p tcp -j REJECT --reject-with tcp-reset" "$MOCK/XR_FWD6" && grep -qx -- "-i wlan1 -o xr0 -j REJECT --reject-with icmp6-adm-prohibited" "$MOCK/XR_FWD6"'
# down 清理全部 IPv6 规则、链与表
rmdir "$T/sysnet/xr0" 2>/dev/null; : > "$MOCK/calls"; $NET down
chk 'grep -q "^ip -6 rule del pref 19100" "$MOCK/calls" && grep -q "^ip -6 route flush table 233" "$MOCK/calls" && grep -q "^ip -6 rule del pref 18800" "$MOCK/calls" && grep -q "^ip -6 rule del pref 18900" "$MOCK/calls" && grep -q "^ip -6 rule del pref 4800" "$MOCK/calls"'
chk '[ ! -f "$MOCK/fwd6_hook" ] && [ ! -f "$MOCK/XR_FWD6" ] && [ ! -f "$RUN/net.v6proxy" ]'
chk 'grep -q "^ndc network route remove local xr0 fd7a:7872:e9::/64" "$MOCK/calls"'

# MTU：xr0 与 OpenWrt eth0 跟随上游（网卡 MTU 与上游默认路由 mtu 取最小），上游切换时重设；SYN 按路径 MTU 钳制 MSS
set_kv XR_EXPOSE lan
mkdir -p "$T/sysnet/rmnet_data2" "$T/sysnet/wlan0"; echo 1500 > "$T/sysnet/rmnet_data2/mtu"; echo 1500 > "$T/sysnet/wlan0/mtu"
echo "default via 10.114.199.189 dev rmnet_data2 proto static mtu 1410" > "$MOCK/rt.rmnet_data2"
echo "11000:	from all fwmark 0x0/0xffff iif lo lookup rmnet_data2" > "$MOCK/iprule"; mkdir -p "$T/sysnet/xr0"; : > "$MOCK/calls"; $NET up
chk 'grep -q "^ip link set xr0 mtu 1410" "$MOCK/calls" && [ "$(cat "$RUN/net.mtu")" = 1410 ] && [ "$(cat "$RUN/guest/host/mtu")" = 1410 ]'
chk 'grep -q "^iptables -t mangle -A XR_MSS -o xr0 -p tcp --tcp-flags SYN,RST SYN -j TCPMSS --clamp-mss-to-pmtu" "$MOCK/calls"'
chk 'grep -q "^iptables -t mangle -A XR_MSS -o rmnet_data2 -p tcp --tcp-flags SYN,RST SYN -j TCPMSS --clamp-mss-to-pmtu" "$MOCK/calls"'
chk 'grep -q "^ip6tables -t mangle -A XR_MSS -o xr0 " "$MOCK/calls" && grep -q "^iptables -t mangle -I POSTROUTING 1 -j XR_MSS" "$MOCK/calls"'
rm -f "$MOCK/rt.rmnet_data2"
echo "11000:	from all fwmark 0x0/0xffff iif lo lookup wlan0" > "$MOCK/iprule"; : > "$MOCK/calls"; $NET sync
chk 'grep -q "^ip link set xr0 mtu 1500" "$MOCK/calls" && [ "$(cat "$RUN/guest/host/mtu")" = 1500 ]'
echo 1280 > "$T/sysnet/wlan0/mtu"; echo 1200 > "$T/sysnet/wlan0/mtu"; : > "$MOCK/calls"; $NET sync
chk 'grep -q "^ip link set xr0 mtu 1280" "$MOCK/calls"'   # 下限 1280（IPv6 最小 MTU）
echo 1500 > "$T/sysnet/wlan0/mtu"; : > "$MOCK/calls"; $NET down
chk 'grep -q "^iptables -t mangle -D POSTROUTING -j XR_MSS" "$MOCK/calls" && grep -q "^iptables -t mangle -X XR_MSS" "$MOCK/calls"'
echo "11000:	from all fwmark 0x0/0xffff iif lo lookup rmnet_data2" > "$MOCK/iprule"

# 调试模式：命令、返回值、错误输出与判断依据写入 debug.log；关闭时不写
rm -f "$XR_DATA/log/debug.log"; : > "$MOCK/calls"; $NET up; $NET down
chk '[ ! -f "$XR_DATA/log/debug.log" ]'
set_kv XR_DEBUG 1; : > "$MOCK/calls"; $NET up
chk 'grep -q "\[cmd\] rc=0 ip addr add 172.29.233.1/24 dev xr0" "$XR_DATA/log/debug.log"'
chk 'grep -q "\[net\] 同步：上游=rmnet_data2" "$XR_DATA/log/debug.log" && grep -q "接管=block（模式=global" "$XR_DATA/log/debug.log"'
chk 'grep -q "IPv6 拦截：" "$XR_DATA/log/debug.log" && grep -q "\[sysctl\] rc=" "$XR_DATA/log/debug.log"'
$NET down
# 没有 ip6 nat 表的机型：只探测一次，之后既不去删 XR_NAT6 挂钩，调试日志里也不出现相关错误
: > "$XR_DATA/log/debug.log"; : > "$MOCK/calls"; NAT6_FAIL=1 $NET up; NAT6_FAIL=1 $NET sync; NAT6_FAIL=1 $NET down
chk '! grep -q "XR_NAT6" "$XR_DATA/log/debug.log" && [ "$(grep -c "^ip6tables .*-t nat" "$MOCK/calls")" -le 1 ]'
sh "$ROOT/module/scripts/xr" set XR_DEBUG 0 >/dev/null

# 对时：控制目录里的 host/time 过期（≥30 分钟）或在未来时才重写，平时不写盘
mkdir -p "$RUN/guest/host"; rm -f "$RUN/guest/host/time"
$NET sync; chk '[ "$(cat "$RUN/guest/host/time")" -ge "$(( $(date +%s) - 5 ))" ]'
echo 123 > "$RUN/guest/host/time"; $NET sync; chk '[ "$(cat "$RUN/guest/host/time")" -gt 123 ]'
t0="$(( $(date +%s) - 60 ))"; echo "$t0" > "$RUN/guest/host/time"; $NET sync; chk '[ "$(cat "$RUN/guest/host/time")" = "$t0" ]'
tf="$(( $(date +%s) + 86400 ))"; echo "$tf" > "$RUN/guest/host/time"; $NET sync; chk '[ "$(cat "$RUN/guest/host/time")" -lt "$tf" ]'

if [ "$fail" = 0 ]; then echo "test-net OK"; else echo "test-net FAILED"; exit 1; fi
