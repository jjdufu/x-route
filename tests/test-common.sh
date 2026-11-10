#!/bin/sh
# shellcheck disable=SC2016,SC2317,SC2034
# x-route: common.sh 单元测试（在 mksh / busybox ash / dash 下运行，模拟 Android 32 位算术）
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT
XR_DATA="$T/data"; MODDIR="$ROOT/module"; export XR_DATA MODDIR
. "$ROOT/module/scripts/common.sh"
ensure_dirs; cp "$ROOT/module/config/config.conf" "$CONF"
fail=0
ok()  { if "$@"; then :; else echo "FAIL: $*"; fail=1; fi; }
no()  { if "$@"; then echo "FAIL(should reject): $*"; fail=1; fi; }

ok valid_kv XR_NET_PREFIX 172.29.233;   no valid_kv XR_NET_PREFIX 8.8.8;   no valid_kv XR_NET_PREFIX 172.29.256
no valid_kv XR_NET_PREFIX 172.32.1;     ok valid_kv XR_NET_PREFIX 192.168.7; ok valid_kv XR_NET_PREFIX 10.1.2
ok valid_kv XR_VM_MAC 52:54:00:1d:e9:fe; no valid_kv XR_VM_MAC 52:54:00:1D:e9:fe
ok valid_kv XR_DNS "223.5.5.5 119.29.29.29"; no valid_kv XR_DNS ""; no valid_kv XR_DNS "1.1.1.1 2.2.2.2 3.3.3.3 4.4.4.4"; no valid_kv XR_DNS "1.1.1.300"
ok valid_kv XR_EXPOSE hotspot; no valid_kv XR_EXPOSE lan
ok valid_kv VM_CPUS 4; no valid_kv VM_CPUS 5; no valid_kv VM_CPUS x
ok valid_kv VM_MEM_MB 512; no valid_kv VM_MEM_MB 256; no valid_kv VM_MEM_MB 99999999
ok valid_kv VM_SWIOTLB_MB 64; no valid_kv VM_SWIOTLB_MB 48
ok valid_kv AUTOSTART 1; no valid_kv AUTOSTART 2
ok valid_kv VM_DISK_SIZE_MB 2048; no valid_kv VM_DISK_SIZE_MB 100
ok valid_kv VM_SHARE_MODE ro; no valid_kv VM_SHARE_MODE rw
( RW_MAP_OPT="uidmap=0 1023 1"; valid_kv VM_SHARE_MODE rw ) || { echo "FAIL: rw with RW_MAP_OPT"; fail=1; }
no valid_kv VM_SHARE_PATH /data/adb; no valid_kv VM_SHARE_PATH /data/media/0/../../adb
ok valid_kv VPN_MODE site; no valid_kv VPN_MODE full
ok valid_kv VPN_ROUTES ""; ok valid_kv VPN_ROUTES "10.20.0.0/16,192.168.50.0/24"; no valid_kv VPN_ROUTES "10.0.0.0/33"; no valid_kv VPN_ROUTES "a;b"
ok valid_kv READY_TIMEOUT_SEC 180; no valid_kv READY_TIMEOUT_SEC 10
no valid_kv UNKNOWN 1

# CIDR（含高位地址，验证 32 位算术下的正确性）
ok cidr_private 10.20.0.0/16; ok cidr_private 172.16.0.0/12; ok cidr_private 192.168.50.0/24; ok cidr_private 100.64.0.0/10
no cidr_private 8.8.8.0/24; no cidr_private 172.32.0.0/16; no cidr_private 192.0.0.0/8; no cidr_private 10.0.0.0/7
ok cidr_aligned 192.168.50.0/24; no cidr_aligned 192.168.50.1/24; ok cidr_aligned 172.16.0.0/12; no cidr_aligned 172.17.0.0/12
ok cidr_overlap 192.168.0.0/16 192.168.50.0/24; no cidr_overlap 192.168.50.0/24 192.168.51.0/24
ok cidr_overlap 172.29.233.0/24 172.16.0.0/12; no cidr_overlap 10.0.0.0/8 172.29.233.0/24

# set_kv：合法值写入，非法值不改文件
ok set_kv XR_EXPOSE off; grep -qx 'XR_EXPOSE="off"' "$CONF" || { echo "FAIL: set_kv write"; fail=1; }
h1="$(sha256sum "$CONF")"; no set_kv XR_EXPOSE bad; [ "$h1" = "$(sha256sum "$CONF")" ] || { echo "FAIL: set_kv modified on invalid"; fail=1; }
ok set_kv VPN_ROUTES "10.20.0.0/16"; load_config; [ "$VPN_ROUTES" = "10.20.0.0/16" ] || { echo "FAIL: reload"; fail=1; }
h="$(config_hash)"; [ "${#h}" -eq 16 ] || { echo "FAIL: config_hash"; fail=1; }
set_kv VM_MEM_MB 1024; [ "$h" != "$(config_hash)" ] || { echo "FAIL: hash unchanged"; fail=1; }
log_info test hello; grep -q "\[INFO\] \[test\] hello" "$LOG" || { echo "FAIL: log"; fail=1; }

[ "$fail" = 0 ] && echo "test-common OK ($(readlink -f /proc/$$/exe 2>/dev/null || echo sh))"
exit "$fail"
