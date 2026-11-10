#!/bin/sh
# shellcheck disable=SC2016,SC2317,SC2034
# XRouter: customize.sh / service.sh / uninstall.sh 的模拟测试（假 MODPATH、假 getprop）
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
T="$(mktemp -d)"; cleanup() {
  if [ -f "${XR_DATA:-}/run/daemon.pid" ]; then
    p="$(cat "$XR_DATA/run/daemon.pid" 2>/dev/null)"
    [ -n "$p" ] && kill "$p" 2>/dev/null
  fi
  if [ -f "$T/live/uninstall.sh" ]; then
    MODDIR="$T/live" sh "$T/live/uninstall.sh" >/dev/null 2>&1
  fi
  rm -rf "$T"
}
trap cleanup EXIT

fail=0; chk() { if eval "$1"; then :; else echo "FAIL: $1"; fail=1; fi; }
mkdir -p "$T/bin"
cat > "$T/bin/getprop" <<'G'
#!/bin/sh
case "$1" in sys.boot_completed) echo 1;; sys.user.0.ce_available) echo true;; esac
G
chmod +x "$T/bin/getprop"; PATH="$T/bin:$PATH"; export PATH
export XR_DATA="$T/data"

install_pkg() { # $1=full|lite
  rm -rf "$T/mod"; cp -a "$ROOT/module" "$T/mod"
  if [ "$1" = full ]; then
    mkdir -p "$T/mod/data"; echo kernel > "$T/mod/data/Image"
    truncate -s 4M "$T/raw"; gzip -c "$T/raw" > "$T/mod/data/openwrt-rootfs.img.gz"
  fi
  ( MODPATH="$T/mod" ARCH=arm64
    ui_print() { echo "$*"; }; abort() { echo "$*"; exit 1; }
    set_perm() { :; }; set_perm_recursive() { :; }
    df() { echo "Filesystem 1M-blocks Used Available"; echo "/dev/x 100000 1 99999 1% /data"; }
    . "$T/mod/customize.sh" ) > "$T/install.log" 2>&1
}

install_pkg full; chk '[ $? -eq 0 ]'
chk '[ -f "$XR_DATA/config/config.conf" ] && [ -f "$XR_DATA/images/Image" ] && [ -f "$XR_DATA/images/openwrt-rootfs.img.gz" ]'
chk '[ "$(stat -c %s "$XR_DATA/images/openwrt-rootfs.img")" -eq $((1024*1024*1024)) ]'
chk '[ ! -d "$T/mod/data" ] && [ -f "$T/mod/skip_mount" ]'
# 重装保留用户数据
sed -i 's/^XR_EXPOSE=.*/XR_EXPOSE="local"/' "$XR_DATA/config/config.conf"; echo keep >> "$XR_DATA/images/openwrt-rootfs.img"
install_pkg full; chk 'grep -q "XR_EXPOSE=\"local\"" "$XR_DATA/config/config.conf" && tail -c5 "$XR_DATA/images/openwrt-rootfs.img" | grep -q keep'
# Lite：已有镜像可装；没有镜像时中止
install_pkg lite; chk 'grep -q "Lite" "$T/install.log" && ! grep -q "^! 缺少" "$T/install.log"'
rm -rf "$XR_DATA"; install_pkg lite; chk 'grep -q "缺少" "$T/install.log"'

# service.sh：熔断文件存在 -> 写 tripped、AUTOSTART=0、不启动
install_pkg full
cp -a "$T/mod" "$T/live"; sed -i 's/^AUTOSTART=.*/AUTOSTART="1"/' "$XR_DATA/config/config.conf"
echo "1 abc" > "$XR_DATA/config/vm_booting.fuse"
MODDIR="$T/live" sh "$T/live/service.sh"
chk '[ -f "$XR_DATA/config/tripped" ] && [ ! -f "$XR_DATA/config/vm_booting.fuse" ] && grep -q "AUTOSTART=\"0\"" "$XR_DATA/config/config.conf"'
# AUTOSTART=1 但 last_good 不匹配 -> 跳过
rm -f "$XR_DATA/config/tripped"; sed -i 's/^AUTOSTART=.*/AUTOSTART="1"/' "$XR_DATA/config/config.conf"
MODDIR="$T/live" sh "$T/live/service.sh"; chk 'grep -q "last_good 与当前配置不一致" "$XR_DATA/log/module.log"'
# 卸载：停止残留进程、删除数据目录；不碰用户导出的日志包；容错已消失的规则
install_pkg full
cp -a "$T/mod" "$T/live"
mkdir -p "$XR_DATA/log" "$XR_DATA/run" "$XR_DATA/images" "$T/dl"
echo logline > "$XR_DATA/log/module.log"
echo img > "$XR_DATA/images/Image"
# 活着的占位进程（模拟守护进程），以及已死 pid
sleep 60 &
livepid=$!
echo "$livepid" > "$XR_DATA/run/daemon.pid"
echo 999999 > "$XR_DATA/run/crosvm.pid"
echo keep > "$T/dl/xrouter-logs-20261110-180900.zip"
# 制造失败路径：xr / net.sh 不可执行时仍须删数据
chmod a-x "$T/live/scripts/xr" "$T/live/scripts/net.sh" 2>/dev/null || true
MODDIR="$T/live" sh "$T/live/uninstall.sh"; un_rc=$?
chk '[ "$un_rc" -eq 0 ]'
chk '[ ! -d "$XR_DATA" ]'
chk '! kill -0 "$livepid" 2>/dev/null'
chk '[ -f "$T/dl/xrouter-logs-20261110-180900.zip" ]'
# 网络兜底清理覆盖 IPv4 与 IPv6（假 ip / iptables / ip6tables 记录调用）
MOCK="$T/mock"; mkdir -p "$MOCK"; export MOCK
XR_SYSBIN="$ROOT/tests/mock/net" MODDIR="$T/live" sh "$T/live/uninstall.sh" >/dev/null 2>&1
for c in "ip6tables -X XR_FWD6" "ip6tables -t nat -X XR_NAT6" "iptables -X XR_FWD" "ip -6 route flush table 233" "ip route flush table 233" "ip route flush table 97 dev xr0"; do
  chk 'grep -qx "$c" "$MOCK/calls"'
done
for p in 4800 4801 18800 18900 19100; do chk 'grep -qx "ip -6 rule del pref $p" "$MOCK/calls" && grep -qx "ip rule del pref $p" "$MOCK/calls"'; done
# 再次卸载：目录已空、工具缺失也不应非 0
MODDIR="$T/live" sh "$T/live/uninstall.sh"
chk '[ $? -eq 0 ]'

[ "$fail" = 0 ] && echo "test-install OK"
[ "$fail" = 0 ] || cat "$T/install.log"
exit "$fail"
