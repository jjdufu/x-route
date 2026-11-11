#!/bin/sh
# shellcheck disable=SC2016,SC2317,SC2034
# XRouter: common.sh 单元测试（在 mksh / busybox ash / dash 下运行，模拟 Android 32 位算术）
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT
XR_DATA="$T/data"; MODDIR="$ROOT/module"; export XR_DATA MODDIR
. "$ROOT/module/scripts/common.sh"
. "$ROOT/module/scripts/v6.sh"
ensure_dirs; cp "$ROOT/module/config/config.conf" "$CONF"
fail=0
ok()  { if "$@"; then :; else echo "FAIL: $*"; fail=1; fi; }
no()  { if "$@"; then echo "FAIL(should reject): $*"; fail=1; fi; }

ok valid_kv XR_NET_PREFIX 172.29.233;   no valid_kv XR_NET_PREFIX 8.8.8;   no valid_kv XR_NET_PREFIX 172.29.256
no valid_kv XR_NET_PREFIX 172.32.1;     ok valid_kv XR_NET_PREFIX 192.168.7; ok valid_kv XR_NET_PREFIX 10.1.2
ok valid_kv XR_VM_MAC 52:54:00:1d:e9:fe; no valid_kv XR_VM_MAC 52:54:00:1D:e9:fe
ok valid_kv XR_DNS "223.5.5.5 119.29.29.29"; no valid_kv XR_DNS ""; no valid_kv XR_DNS "1.1.1.1 2.2.2.2 3.3.3.3 4.4.4.4"; no valid_kv XR_DNS "1.1.1.300"
ok valid_kv XR_EXPOSE lan; ok valid_kv XR_EXPOSE local; no valid_kv XR_EXPOSE hotspot; no valid_kv XR_EXPOSE off
ok valid_kv XR_MODE global; ok valid_kv XR_MODE bypass; no valid_kv XR_MODE site
ok valid_kv VM_CPUS 4; no valid_kv VM_CPUS 5; no valid_kv VM_CPUS x
ok valid_kv VM_MEM_MB 512; no valid_kv VM_MEM_MB 256; no valid_kv VM_MEM_MB 99999999
ok valid_kv VM_SWIOTLB_MB 64; no valid_kv VM_SWIOTLB_MB 48
ok valid_kv AUTOSTART 1; no valid_kv AUTOSTART 2
ok valid_kv VM_DISK_SIZE_MB 2048; no valid_kv VM_DISK_SIZE_MB 100
ok valid_kv VM_SHARE_MODE ro; no valid_kv VM_SHARE_MODE rw
( RW_MAP_OPT="uidmap=0 1023 1"; valid_kv VM_SHARE_MODE rw ) || { echo "FAIL: rw with RW_MAP_OPT"; fail=1; }
no valid_kv VM_SHARE_PATH /data/adb; no valid_kv VM_SHARE_PATH /data/media/0/../../adb
no valid_kv VPN_MODE site; no valid_kv VPN_ROUTES "10.20.0.0/16"
ok valid_kv READY_TIMEOUT_SEC 180; no valid_kv READY_TIMEOUT_SEC 10
no valid_kv UNKNOWN 1

# CIDR（含高位地址，验证 32 位算术下的正确性）
ok cidr_private 10.20.0.0/16; ok cidr_private 172.16.0.0/12; ok cidr_private 192.168.50.0/24; ok cidr_private 100.64.0.0/10
no valid_kv VPN_ROUTES "8.8.8.0/24"; no valid_kv VPN_ROUTES "192.168.50.1/24"; no cidr_private 8.8.8.0/24; no cidr_private 172.32.0.0/16; no cidr_private 192.0.0.0/8; no cidr_private 10.0.0.0/7
ok cidr_aligned 192.168.50.0/24; no cidr_aligned 192.168.50.1/24; ok cidr_aligned 172.16.0.0/12; no cidr_aligned 172.17.0.0/12
ok cidr_overlap 192.168.0.0/16 192.168.50.0/24; no cidr_overlap 192.168.50.0/24 192.168.51.0/24
ok cidr_overlap 172.29.233.0/24 172.16.0.0/12; no cidr_overlap 10.0.0.0/8 172.29.233.0/24

# OpenVPN 路由规范化：裸主机、点分掩码、IPv6；非法项拒绝
ok valid_cidr 10.58.88.2; ok valid_cidr 10.58.88.2/32; ok valid_cidr 10.8.0.0/24
ok valid_cidr 8.8.8.0/24; ok valid_cidr 10.1.2.3/255.255.255.255
[ "$(norm_route 10.58.88.2)" = "10.58.88.2/32" ] || { echo "FAIL: norm bare v4"; fail=1; }
[ "$(norm_route 10.8.0.0/255.255.255.0)" = "10.8.0.0/24" ] || { echo "FAIL: norm mask"; fail=1; }
ok valid_cidr 2001:db8::1; ok valid_cidr 2001:db8::1/128; ok valid_cidr 2001:db8:1::/64
[ "$(norm_route 2001:db8::1)" = "2001:db8::1/128" ] || { echo "FAIL: norm bare v6"; fail=1; }
[ "$(ip6_expand 2001:db8::1)" = "20010db8000000000000000000000001" ] || { echo "FAIL: ip6_expand"; fail=1; }
[ "$(ip6_expand ::1)" = "00000000000000000000000000000001" ] || { echo "FAIL: ip6_expand ::1"; fail=1; }
ok cidr_aligned 10.58.88.2; ok cidr_aligned 10.58.88.2/32; ok cidr_aligned 2001:db8::1
ok cidr_aligned 2001:db8:1::/64; no cidr_aligned 2001:db8:1::1/64; no cidr_aligned 10.9.0.1/24
ok cidr_overlap 10.58.88.2 10.58.88.0/24; no cidr_overlap 10.58.88.2 10.58.89.0/24
ok cidr_overlap 2001:db8:1::/64 2001:db8:1::5/128; no cidr_overlap 2001:db8:1::/64 2001:db8:2::/64
no cidr_overlap 10.58.88.2 2001:db8::1
no valid_cidr bad; no valid_cidr 10.0.0.1/99; no valid_cidr 999.1.1.1; no valid_cidr gggg::1
no valid_cidr 10.0.0.0/0; no valid_cidr 2001:db8::/129; no valid_cidr 2001:db8:::1
no valid_cidr 10.8.0.0/255.0.255.0

# IPv6 前缀共享：EUI-64 与选址（避开已占用地址）
[ "$(mac_eui64 52:54:00:1d:e9:fe)" = "505400fffe1de9fe" ] || { echo "FAIL: mac_eui64"; fail=1; }
[ "$(v6_compose 2409:8a00:1:2::/64 505400fffe1de9fe)" = "2409:8a00:0001:0002:5054:00ff:fe1d:e9fe" ] || { echo "FAIL: v6_compose"; fail=1; }
_pick="$(v6_pick_addr 2409:8a00:1:2::/64 "2409:8a00:1:2:1111:2222:3333:4444/64" 52:54:00:1d:e9:fe)"
[ "$_pick" = "2409:8a00:0001:0002:5054:00ff:fe1d:e9fe/64" ] || { echo "FAIL: v6_pick_addr $_pick"; fail=1; }
_busy="2409:8a00:0001:0002:5054:00ff:fe1d:e9fe/64"
_pick="$(v6_pick_addr 2409:8a00:1:2::/64 "$_busy" 52:54:00:1d:e9:fe)"
[ "$_pick" = "2409:8a00:0001:0002:7872:00e9:0000:0254/64" ] || { echo "FAIL: v6_pick_addr fallback $_pick"; fail=1; }
if v6_is_ll fe80::1 && ! v6_is_ll 2409:8a00:1:2::1 && v6_is_ula fd7a:7872:e9::1 && ! v6_is_ula 2409:8a00:1:2::1; then
  :
else
  echo "FAIL: v6_is_ll/ula"; fail=1
fi

# set_kv：合法值写入，非法值不改文件
ok set_kv XR_EXPOSE local; grep -qx 'XR_EXPOSE="local"' "$CONF" || { echo "FAIL: set_kv write"; fail=1; }
h1="$(sha256sum "$CONF")"; no set_kv XR_EXPOSE bad; [ "$h1" = "$(sha256sum "$CONF")" ] || { echo "FAIL: set_kv modified on invalid"; fail=1; }
ok set_kv XR_MODE bypass; load_config; [ "$XR_MODE" = "bypass" ] || { echo "FAIL: reload"; fail=1; }
h="$(config_hash)"; [ "${#h}" -eq 16 ] || { echo "FAIL: config_hash"; fail=1; }
set_kv VM_MEM_MB 1024; [ "$h" != "$(config_hash)" ] || { echo "FAIL: hash unchanged"; fail=1; }
log_info test hello; grep -q "\[INFO\] \[test\] hello" "$LOG" || { echo "FAIL: log"; fail=1; }


# /sdcard、/storage/emulated/0 视为 MEDIA
mkdir -p "$T/media/DCIM"
MEDIA="$T/media"
[ "$(canon_share_path /sdcard)" = "$T/media" ] || { echo "FAIL: /sdcard -> MEDIA"; fail=1; }
[ "$(canon_share_path /sdcard/DCIM)" = "$T/media/DCIM" ] || { echo "FAIL: /sdcard/DCIM"; fail=1; }
[ "$(canon_share_path /storage/emulated/0)" = "$T/media" ] || { echo "FAIL: /storage/emulated/0"; fail=1; }
[ "$(canon_share_path /storage/emulated/0/DCIM)" = "$T/media/DCIM" ] || { echo "FAIL: emulated DCIM"; fail=1; }
[ "$(canon_share_path "$T/media/DCIM")" = "$T/media/DCIM" ] || { echo "FAIL: already media path"; fail=1; }
ok set_kv VM_SHARE_PATH /sdcard
grep -qx "VM_SHARE_PATH=\"$T/media\"" "$CONF" || { echo "FAIL: set_kv /sdcard 未写成 MEDIA"; fail=1; }
ok set_kv VM_SHARE_PATH /sdcard/DCIM
grep -qx "VM_SHARE_PATH=\"$T/media/DCIM\"" "$CONF" || { echo "FAIL: set_kv /sdcard/DCIM"; fail=1; }

# 日志轮转：超过 512KB 后改名为 .1，新文件重新写
dd if=/dev/zero bs=1024 count=600 2>/dev/null | tr '\0' 'x' > "$LOG"
log_info rotate hello
[ -f "${LOG}.1" ] || { echo "FAIL: 未生成 ${LOG}.1"; fail=1; }
sz="$(stat -c %s "$LOG" 2>/dev/null || echo 0)"
[ "$sz" -lt 524288 ] || { echo "FAIL: 轮转后 $LOG 仍过大 ($sz)"; fail=1; }
grep -q "hello" "$LOG" || { echo "FAIL: 新日志未写入"; fail=1; }

[ "$fail" = 0 ] && echo "test-common OK ($(readlink -f /proc/$$/exe 2>/dev/null || echo sh))"
exit "$fail"
