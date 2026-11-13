#!/bin/sh
# shellcheck disable=SC2016,SC2317,SC2034
# XRouter: 热点接管（hotspot.rs）模拟测试：跟随安卓网段 / 固定网段 / 冲突退回 / 改设置后重新桥接
# 假 ip / iptables（tests/mock/net），客侧由后台小循环模拟：读 host/hotspot.cmd，按前两段回写 guest/hotspot
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
T="$(mktemp -d)"; MOCK="$T/mock"; mkdir -p "$MOCK"
fail=0; chk() { if eval "$1"; then :; else echo "FAIL: $1"; fail=1; fi; }
export PATH="$ROOT/tests/mock/net:$PATH" MOCK XR_DATA="$T/data" MODDIR="$ROOT/module" XR_SYSBIN="$ROOT/tests/mock/net" XR_PROC="$T/proc" XR_PROC_BASE="$T/proc" XR_SYSNET="$T/sysnet"
CONF="$XR_DATA/config/config.conf"; RUN="$XR_DATA/run"; LOG="$XR_DATA/log/module.log"
mkdir -p "$XR_DATA/config" "$RUN" "$XR_DATA/log" "$T/proc/sys/net/ipv4/conf/xr0" "$T/proc/sys/net/ipv6/conf/xr0"
echo 0 > "$T/proc/sys/net/ipv4/ip_forward"
cp "$ROOT/module/config/config.conf" "$CONF"
NET="sh $ROOT/module/scripts/net.sh"
XR="sh $ROOT/module/scripts/xr"

# 虚拟机视为就绪；手机侧网卡：wlan1 = 热点（Wi-Fi、up），xr1 = 虚拟机第二块网卡
mkdir -p "$T/proc/4321"
printf 'crosvm\0run\0' > "$T/proc/4321/cmdline"
echo '4321 (crosvm) S 1 1 1 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 1 1 0' > "$T/proc/4321/stat"
echo 4321 > "$RUN/crosvm.pid"
echo ready > "$RUN/state"
mkdir -p "$T/sysnet/wlan1/wireless" "$T/sysnet/xr1" "$T/sysnet/xr0"
echo up > "$T/sysnet/wlan1/operstate"
echo "11000:	from all fwmark 0x0/0xffff iif lo lookup rmnet_data2" > "$MOCK/iprule"
echo "192.168.43.0/24 dev wlan1 proto static scope link" > "$MOCK/lnroutes"
echo "30: wlan1    inet 192.168.43.1/24 brd 192.168.43.255 scope global wlan1" > "$MOCK/addr.wlan1"
mkdir -p "$RUN/guest/host" "$RUN/guest/guest"; echo off > "$RUN/guest/guest/hotspot"
( while [ -d "$RUN/guest" ]; do
    c="$(cat "$RUN/guest/host/hotspot.cmd" 2>/dev/null)"
    if [ -n "$c" ] && [ "$c" != "${last:-}" ]; then last="$c"; echo "${c% *}" | awk '{print $1, $2}' | sed 's/ $//' > "$RUN/guest/guest/hotspot"; fi
    sleep 0.1
  done ) &
G=$!
trap 'kill $G 2>/dev/null; [ -n "${KEEP:-}" ] || rm -rf "$T"' EXIT
# 桥接后由 ip link set master 产生的 master 链接（假 ip 不会建）
bridged() { ln -sfn ../xrbr1 "$T/sysnet/wlan1/master"; ln -sfn ../xrbr1 "$T/sysnet/xr1/master"; mkdir -p "$T/sysnet/xrbr1"; }
unbridged() { rm -rf "$T/sysnet/wlan1/master" "$T/sysnet/xr1/master" "$T/sysnet/xrbr1"; }

# 1. 跟随安卓（XR_HOTSPOT_SUBNET 为空）：OpenWrt 接管安卓网关，不加 4790 规则
$XR internal set-kv XR_HOTSPOT_SUBNET "" >/dev/null; : > "$MOCK/calls"
$NET sync; bridged
chk 'grep -q "^on 192.168.43.1/24 " "$RUN/guest/host/hotspot.cmd" && ! grep -q "+" "$RUN/guest/host/hotspot.cmd"'
chk '[ "$(cat "$RUN/net.hs_gw")" = 192.168.43.1/24 ] && [ "$(cat "$RUN/net.hs_cidr")" = 192.168.43.1/24 ]'
chk '! grep -q "rule add pref 4790" "$MOCK/calls"'
# 已桥接、设置未变：幂等，不重新下发
c0="$(cat "$RUN/guest/host/hotspot.cmd")"; $NET sync; chk '[ "$(cat "$RUN/guest/host/hotspot.cmd")" = "$c0" ]'

# 2. 改为固定网段：自动拆桥重桥，OpenWrt 用固定网段并保留安卓原网关；手机经 xr0 访问固定网段
$XR internal set-kv XR_HOTSPOT_SUBNET "10.233.0.1/24" >/dev/null; : > "$MOCK/calls"
$NET sync; bridged
chk 'grep -q "^on 10.233.0.1/24 +192.168.43.1/24 " "$RUN/guest/host/hotspot.cmd"'
chk '[ "$(cat "$RUN/net.hs_gw")" = 10.233.0.1/24 ] && [ "$(cat "$RUN/net.hs_cidr")" = 192.168.43.1/24 ]'
chk 'grep -q "^ip route replace 10.233.0.0/24 via 172.29.233.254 dev xr0 table 234" "$MOCK/calls"'
chk 'grep -q "^ip rule add pref 4790 to 10.233.0.0/24 lookup 234" "$MOCK/calls"'
chk 'grep -q "固定网段 10.233.0.1/24" "$LOG"'
c0="$(cat "$RUN/guest/host/hotspot.cmd")"; $NET sync; chk '[ "$(cat "$RUN/guest/host/hotspot.cmd")" = "$c0" ]'

# 3. 固定网段与上游冲突（比如连上了同网段的 Wi-Fi）：本次退回跟随安卓，并只记一次错误
echo "20: wlan0    inet 10.233.0.88/24 scope global wlan0" > "$T/addr.all"
mkdir -p "$T/sb"; cp -a "$ROOT/tests/mock/net/." "$T/sb/"
printf '#!/bin/sh\ncase "$*" in "-4 -o addr show") cat "%s/addr.all" "%s/addr.wlan1" ;; *) exec "%s/ip" "$@" ;; esac\n' "$T" "$MOCK" "$ROOT/tests/mock/net" > "$T/sb/ip"
chmod +x "$T/sb/ip"
unbridged; : > "$MOCK/calls"
XR_SYSBIN="$T/sb" $NET sync; bridged
chk 'grep -q "^on 192.168.43.1/24 " "$RUN/guest/host/hotspot.cmd" && [ "$(cat "$RUN/net.hs_gw")" = 192.168.43.1/24 ]'
chk '[ "$(grep -c "与 wlan0 10.233.0.88/24 冲突" "$LOG")" = 1 ]'
XR_SYSBIN="$T/sb" $NET sync; chk '[ "$(grep -c "与 wlan0 10.233.0.88/24 冲突" "$LOG")" = 1 ]'

# 4. 拆桥：关闭热点桥接后，清掉 4790 规则与表 234，记录文件全部删除
$XR internal set-kv XR_HOTSPOT_SUBNET "10.233.0.1/24" >/dev/null
unbridged; $NET sync; bridged
: > "$MOCK/calls"
$XR internal set-kv XR_HOTSPOT_BRIDGE 0 >/dev/null
$NET sync
chk 'grep -q "^ip route flush table 234" "$MOCK/calls" && [ ! -f "$RUN/net.hs_gw" ] && [ ! -f "$RUN/net.hs_if" ]'

# 5. 客侧脚本：okcidr 与 +旧网关参数解析（不依赖 uci 的部分）
OW="$ROOT/openwrt/files/usr/libexec/xrouter-hotspot"
eval "$(sed -n '/^okcidr()/,/^}/p; /^net_of()/,/^}/p' "$OW")"
chk 'okcidr 10.233.0.1/24 && okcidr 192.168.43.1/30 && ! okcidr 10.233.0.1/31 && ! okcidr 10.233.0.1 && ! okcidr "10.233.0.1/24;x" && ! okcidr 10.233.0.256/24'
chk '[ "$(net_of 10.233.0.1 24)" = 10.233.0.0/24 ]'

if [ "$fail" = 0 ]; then echo "test-hotspot OK"; else echo "test-hotspot FAILED"; exit 1; fi
