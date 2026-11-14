#!/bin/sh
# shellcheck disable=SC2016,SC2317,SC2034
# XRouter: 配置校验与哈希 CLI 测试（经 scripts/xr -> bin/xr；CIDR 与 IPv6 算法由 cargo test 覆盖）
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT
XR_DATA="$T/data"; MODDIR="$ROOT/module"; export XR_DATA MODDIR
CONF="$XR_DATA/config/config.conf"
mkdir -p "$XR_DATA/config" "$XR_DATA/run" "$XR_DATA/log"
cp "$ROOT/module/config/config.conf" "$CONF"
xr() { sh "$ROOT/module/scripts/xr" "$@"; }
fail=0
ok()  { if xr internal "$@"; then :; else echo "FAIL: $*"; fail=1; fi; }
no()  { if xr internal "$@"; then echo "FAIL(should reject): $*"; fail=1; fi; }

ok valid-kv XR_NET_PREFIX 172.29.233;   no valid-kv XR_NET_PREFIX 8.8.8;   no valid-kv XR_NET_PREFIX 172.29.256
no valid-kv XR_NET_PREFIX 172.32.1;     ok valid-kv XR_NET_PREFIX 192.168.7; ok valid-kv XR_NET_PREFIX 10.1.2
ok valid-kv XR_VM_MAC 52:54:00:1d:e9:fe; no valid-kv XR_VM_MAC 52:54:00:1D:e9:fe
ok valid-kv XR_DNS "223.5.5.5 119.29.29.29"; no valid-kv XR_DNS ""; no valid-kv XR_DNS "1.1.1.1 2.2.2.2 3.3.3.3 4.4.4.4"; no valid-kv XR_DNS "1.1.1.300"
no valid-kv XR_DNS "192.168.01.1"
ok valid-kv XR_EXPOSE lan; ok valid-kv XR_EXPOSE local; no valid-kv XR_EXPOSE hotspot; no valid-kv XR_EXPOSE off
ok valid-kv XR_MODE global; ok valid-kv XR_MODE bypass; no valid-kv XR_MODE site
ok valid-kv VM_CPUS 4; no valid-kv VM_CPUS 5; no valid-kv VM_CPUS x
ok valid-kv VM_MEM_MB 512; no valid-kv VM_MEM_MB 256; no valid-kv VM_MEM_MB 99999999
ok valid-kv VM_SWIOTLB_MB 64; no valid-kv VM_SWIOTLB_MB 48
ok valid-kv AUTOSTART 1; no valid-kv AUTOSTART 2
ok valid-kv VM_DISK_SIZE_MB 2048; no valid-kv VM_DISK_SIZE_MB 100
ok valid-kv VM_SHARE_MODE ro; no valid-kv VM_SHARE_MODE rw
( export RW_MAP_OPT="uidmap=0 1023 1"; xr internal valid-kv VM_SHARE_MODE rw ) || { echo "FAIL: rw with RW_MAP_OPT"; fail=1; }
no valid-kv VM_SHARE_PATH /data/adb; no valid-kv VM_SHARE_PATH /data/media/0/../../adb
ok valid-kv READY_TIMEOUT_SEC 180; no valid-kv READY_TIMEOUT_SEC 10
no valid-kv UNKNOWN 1

# 固定热点网段（模拟 ip 输出）
mkdir -p "$T/sb"
cat > "$T/sb/ip" <<'IPM'
#!/bin/sh
case "$*" in
  "-4 -o addr show") printf '%s\n' "1: lo    inet 127.0.0.1/8 scope host lo" "20: wlan0    inet 192.168.31.88/24 brd 192.168.31.255 scope global wlan0" \
      "21: rmnet_data1    inet 10.120.5.6/30 scope global rmnet_data1" "30: wlan2    inet 10.233.0.47/24 scope global wlan2" "40: xr0    inet 172.29.233.1/24 scope global xr0" ;;
  "route show table local_network") echo "10.233.0.0/24 dev wlan2 proto static scope link" ;;
esac
IPM
chmod +x "$T/sb/ip"
OLD_SB="${XR_SYSBIN:-}"; export XR_SYSBIN="$T/sb"
ok valid-kv XR_HOTSPOT_SUBNET ""; ok valid-kv XR_HOTSPOT_SUBNET 10.234.0.1/24; ok valid-kv XR_HOTSPOT_SUBNET 192.168.77.254/24
ok valid-kv XR_HOTSPOT_SUBNET 172.20.0.1/16; ok valid-kv XR_HOTSPOT_SUBNET 10.9.9.1/30
no valid-kv XR_HOTSPOT_SUBNET 10.234.0.0/24; no valid-kv XR_HOTSPOT_SUBNET 10.234.0.255/24; no valid-kv XR_HOTSPOT_SUBNET 10.234.0.1/15
no valid-kv XR_HOTSPOT_SUBNET 10.234.0.1/31; no valid-kv XR_HOTSPOT_SUBNET 8.8.8.1/24; no valid-kv XR_HOTSPOT_SUBNET 100.64.0.1/24
no valid-kv XR_HOTSPOT_SUBNET 10.234.0.1; no valid-kv XR_HOTSPOT_SUBNET "10.234.0.1/24 x"; no valid-kv XR_HOTSPOT_SUBNET 10.234.0.300/24
no valid-kv XR_HOTSPOT_SUBNET 172.29.233.9/24
no valid-kv XR_HOTSPOT_SUBNET 172.29.0.1/16
no valid-kv XR_HOTSPOT_SUBNET 192.168.31.1/24
no valid-kv XR_HOTSPOT_SUBNET 10.120.5.5/24
ok valid-kv XR_HOTSPOT_SUBNET 10.233.0.1/24
export XR_SYSBIN="$OLD_SB"

# set-kv 与 config-hash
ok set-kv XR_EXPOSE local
grep -qx 'XR_EXPOSE="local"' "$CONF" || { echo "FAIL: set-kv"; fail=1; }
h1="$(xr internal config-hash)"
ok set-kv VM_MEM_MB 1024
h2="$(xr internal config-hash)"
if [ -z "$h1" ] || [ "$h1" = "$h2" ]; then echo "FAIL: config-hash did not change"; fail=1; fi

[ "$fail" = 0 ] && echo "test-common OK"
exit "$fail"
