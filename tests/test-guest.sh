#!/bin/sh
# shellcheck disable=SC2016,SC2317,SC2034
# XRouter: 虚拟机内脚本（xrouter-tunmtu / xrouter-vpnstat / xrouter-acct）模拟测试：假 ip / nft / logger，假 /sys/class/net
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LX="$ROOT/openwrt/files/usr/libexec"
T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT
fail=0; chk() { if eval "$1"; then :; else echo "FAIL: $1"; fail=1; fi; }
mkdir -p "$T/bin" "$T/sys/eth0" "$T/ctl/host" "$T/ctl/guest"
export XRG_SYSNET="$T/sys" XRG_CTL="$T/ctl" XRG_TUN="$T/tun" XRG_TMP="$T/vs" PATH="$T/bin:$PATH" MOCK="$T"
echo 1500 > "$T/sys/eth0/mtu"
cat > "$T/bin/ip" <<'EOF'
#!/bin/sh
echo "ip $*" >> "$MOCK/ip.calls"
case "$*" in
  "link set dev "*" mtu "*) echo "$6" > "$XRG_SYSNET/$4/mtu" ;;
  "-4 -o addr show dev tun0") echo "5: tun0    inet 10.8.0.5/24 scope global tun0" ;;
  "-4 -o addr show dev tun1") ;;
  "-6 -o addr show dev tun1 scope global") echo "6: tun1    inet6 fd00:9::6/64 scope global" ;;
  "-4 route show dev tun0") printf '%s\n' "0.0.0.0/1 via 10.8.0.1" "10.8.0.0/24 proto kernel" "192.168.31.0/24 via 10.8.0.1" ;;
  "-4 route show dev tun1") echo "192.168.50.0/24 via 10.9.0.1" ;;
esac
exit 0
EOF
cat > "$T/bin/nft" <<'EOF'
#!/bin/sh
echo "nft $*" >> "$MOCK/nft.calls"
[ "$1" = -f ] && cp "$2" "$MOCK/nft.last"
[ "$1" = list ] && [ -f "$MOCK/nft.last" ] && exit 0
[ "$1" = list ] && exit 1
exit 0
EOF
printf '#!/bin/sh\necho "$*" >> "$MOCK/logger.calls"\n' > "$T/bin/logger"
printf '#!/bin/sh\nexit 1\n' > "$T/bin/pidof"
printf '#!/bin/sh\nexit 1\n' > "$T/bin/uci"
chmod +x "$T/bin/"*
TM="sh $LX/xrouter-tunmtu"; VS="sh $LX/xrouter-vpnstat"
# busybox sh（独立 applet 模式）优先用自带的 ip / nft 等，假命令不生效：跳过
[ "$(command -v ip)" = "$T/bin/ip" ] || { echo "test-guest skip (applets shadow mocks)"; exit 0; }

# 1. 每条 tun* 各按外层协议扣封装开销（不写死 tun0）；上游 MTU 取手机侧 host/mtu
mkdir -p "$T/sys/tun0" "$T/sys/tun1"; echo 1500 > "$T/sys/tun0/mtu"; echo 1500 > "$T/sys/tun1/mtu"
echo 1410 > "$T/ctl/host/mtu"
$TM proto tun0 udp4; $TM proto tun1 tcp6; $TM apply
chk '[ "$(cat "$T/sys/tun0/mtu")" = 1342 ] && [ "$(cat "$T/sys/tun1/mtu")" = 1308 ]'
chk '[ "$($TM show | tr "\n" ";")" = "tun0 1342 1302 1282;tun1 1308 1268 1248;" ]'
# MSS 钳制：独立表原子替换（先建再删再定义），进出每条隧道、IPv4 / IPv6 分开
chk 'head -n 3 "$T/nft.last" | tr "\n" ";" | grep -qx "table inet xrouter_tun;delete table inet xrouter_tun;table inet xrouter_tun {;"'
chk 'grep -q "oifname \"tun0\" meta nfproto ipv4 tcp flags & (syn|rst) == syn tcp option maxseg size > 1302 tcp option maxseg size set 1302" "$T/nft.last"'
chk 'grep -q "iifname \"tun1\" meta nfproto ipv6 .* size set 1248" "$T/nft.last"'
chk '[ "$(grep -c "priority mangle" "$T/nft.last")" = 2 ] && ! sed -n "/chain output/,/}/p" "$T/nft.last" | grep -q iifname'
# 内容不变不重复加载
n="$(grep -c "^nft -f" "$T/nft.calls")"; $TM apply; chk '[ "$(grep -c "^nft -f" "$T/nft.calls")" = "$n" ]'
# 2. 上游变化：只改 MTU 不重启；外层未知按 udp6；下限 1280
echo 1500 > "$T/ctl/host/mtu"; $TM proto tun1 bogus; $TM apply
chk '[ "$(cat "$T/sys/tun0/mtu")" = 1432 ] && [ "$(cat "$T/sys/tun1/mtu")" = 1412 ]'
echo 1300 > "$T/ctl/host/mtu"; $TM apply; chk '[ "$(cat "$T/sys/tun0/mtu")" = 1280 ]'
echo 9000 > "$T/ctl/host/mtu"; echo 1400 > "$T/sys/eth0/mtu"; $TM apply; chk '[ "$(cat "$T/sys/tun0/mtu")" = 1432 ]'
rm -f "$T/ctl/host/mtu"; $TM apply; chk '[ "$(cat "$T/sys/tun0/mtu")" = 1332 ]'
# 隧道消失：清掉记录与规则
rm -rf "$T/sys/tun0"; $TM apply; chk '[ ! -f "$T/tun/tun0.proto" ] && ! grep -q tun0 "$T/nft.last"'
mkdir -p "$T/sys/tun0"; echo 1500 > "$T/sys/tun0/mtu"
chk '$TM bad >/dev/null; [ $? -eq 2 ]'

# 3. 隧道上报：每条一行（名称、地址、MTU、路由；分半默认路由不上报），汇总兼容旧版
echo home > "$T/tun/tun0.name"
$VS
chk '[ "$(cat "$T/ctl/guest/openvpn.status")" = up ] && [ "$(cat "$T/ctl/guest/openvpn.addr")" = 10.8.0.5/24 ] && [ "$(cat "$T/ctl/guest/openvpn.if")" = tun0 ]'
chk '[ "$(sed -n 1p "$T/ctl/guest/openvpn.tunnels")" = "tun0|home|10.8.0.5/24|1500|10.8.0.0/24,192.168.31.0/24" ]'
chk '[ "$(sed -n 2p "$T/ctl/guest/openvpn.tunnels")" = "tun1|tun1|fd00:9::6/64|1312|192.168.50.0/24" ]'
chk '[ "$(sort "$T/ctl/guest/openvpn.routes" | tr "\n" " ")" = "10.8.0.0/24 192.168.31.0/24 192.168.50.0/24 " ]'
# 内容没变不重写（避免刷闪存）
touch -d "2001-01-01" "$T/ctl/guest/openvpn.tunnels"; $VS; chk '[ "$(date -r "$T/ctl/guest/openvpn.tunnels" +%Y)" = 2001 ]'
rm -rf "$T/sys/tun0" "$T/sys/tun1"; $VS
chk '[ "$(cat "$T/ctl/guest/openvpn.status")" = down ] && [ ! -s "$T/ctl/guest/openvpn.tunnels" ] && [ ! -s "$T/ctl/guest/openvpn.routes" ]'

# 4. 分来源流量计数表：手机 / 热点 × 公网 / 内网
rm -f "$T/nft.last"; sh "$LX/xrouter-acct" init
chk 'grep -q "^table inet xrouter_acct {" "$T/nft.last" && [ "$(grep -c "^  counter " "$T/nft.last")" = 8 ] && grep -q "hook forward priority -5" "$T/nft.last"'
chk 'grep -q "iifname \"eth1\" oifname \"tun\*\" counter name hs_lan_up return" "$T/nft.last" && grep -q "ip saddr 172.29.233.1 counter name ph_wan_up return" "$T/nft.last"'
chk '! grep -qE "chain (fwd|forward) " "$T/nft.last"'   # fwd 是 nft 关键字
cat > "$T/bin/nft" <<'EOF'
#!/bin/sh
[ "$1" = list ] && [ "$2" = counters ] && printf 'table inet xrouter_acct {\n\tcounter ph_wan_up {\n\t\tpackets 3 bytes 100\n\t}\n\tcounter hs_lan_dn {\n\t\tpackets 9 bytes 4096\n\t}\n}\n'
exit 0
EOF
chk '[ "$(sh "$LX/xrouter-acct" show)" = "100 0 0 0 0 0 0 4096" ]'

if [ "$fail" = 0 ]; then echo "test-guest OK"; else echo "test-guest FAILED"; exit 1; fi
