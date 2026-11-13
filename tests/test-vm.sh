#!/bin/sh
# shellcheck disable=SC2016,SC2317,SC2034
# XRouter: vm.sh 模拟测试（假 crosvm / 假 net.sh / 假 getprop）
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
T="$(mktemp -d)"
fail=0; chk() { if eval "$1"; then :; else echo "FAIL: $1"; fail=1; fi; }
mkdir -p "$T/bin" "$T/mod"
cp -a "$ROOT/module/." "$T/mod/"
printf '#!/bin/sh\ncase "$1" in sys.boot_completed) echo 1;; sys.user.0.ce_available) echo true;; esac\n' > "$T/bin/getprop"
chmod +x "$T/bin/getprop"
# 假 net.sh：只记录调用
printf '#!/bin/sh\necho "net $*" >> "%s/net.calls"\n' "$T" > "$T/mod/scripts/net.sh"
export XR_DAEMON_FOREGROUND=1 PATH="$T/bin:$PATH" XR_DATA="$T/data" MODDIR="$T/mod" CROSVM="$ROOT/tests/mock/crosvm" FAKE_ARGS="$T/args"
trap 'sh "$T/mod/scripts/vm.sh" stop >/dev/null 2>&1; rm -rf "$T"' EXIT
CONF="$XR_DATA/config/config.conf"; IMG="$XR_DATA/images"; ROOTFS="$IMG/openwrt-rootfs.img"; RUN="$XR_DATA/run"; LOG="$XR_DATA/log/module.log"
FUSE="$XR_DATA/config/vm_booting.fuse"; LAST_GOOD="$XR_DATA/config/last_good"; TRIPPED="$XR_DATA/config/tripped"
mkdir -p "$XR_DATA/config" "$IMG" "$RUN" "$XR_DATA/log"
cp "$T/mod/config/config.conf" "$CONF"
sed -i 's/^READY_TIMEOUT_SEC=.*/READY_TIMEOUT_SEC="6"/; s/^STABLE_SEC=.*/STABLE_SEC="5"/' "$CONF"
echo k > "$IMG/Image"; truncate -s 300M "$ROOTFS"; truncate -s 1M "$T/r"; gzip -c "$T/r" > "$IMG/openwrt-rootfs.img.gz"
VM="sh $T/mod/scripts/vm.sh"

# 1. 正常启动 -> 就绪 -> 稳定 -> last_good（且刷新 host/time、清理上一次客侧 openvpn 残留）
mkdir -p "$RUN/guest/host" "$RUN/guest/guest"
echo 1000000000 > "$RUN/guest/host/time"
echo up > "$RUN/guest/guest/openvpn.status"
$VM start; chk '[ $? -eq 0 ]'
chk '[ "$(cat "$RUN/guest/host/time")" -gt 1700000000 ] && [ ! -f "$RUN/guest/guest/openvpn.status" ]'
chk 'grep -qx -- "--no-balloon" "$T/args" && grep -qx -- "--protected-vm-without-firmware" "$T/args"'
chk 'grep -qx -- "--swiotlb" "$T/args" && grep -qx "64" "$T/args" && grep -qx "768" "$T/args"'
chk 'grep -q "hardware=virtio-console" "$T/args" && grep -q "xr.ip=172.29.233.254/24 xr.gw=172.29.233.1 xr.dns=223.5.5.5,119.29.29.29 xr.ip6=fd7a:7872:e9::254/64 xr.gw6=fd7a:7872:e9::1 xr.share=off" "$T/args"'
chk 'grep -qE "xr\.time=[0-9]{9,}" "$T/args"'   # 开机把手机时间带给虚拟机
chk 'grep -q "^tap-name=xr0,mac=52:54:00:1d:e9:fe$" "$T/args" && grep -q "net up" "$T/net.calls"'
chk 'grep -q "type=unix-stream,hardware=virtio-console,num=2,path=.*clash-ctl\.sock,input-unix-stream" "$T/args"'
chk 'grep -q "xrctl:type=fs:cache=auto:timeout=0" "$T/args"'
chk '[ -S "$RUN/clash-ctl.sock" ] && [ -S "$RUN/clash-rpc.sock" ]'
chk 'grep -q -- "--shared-dir" "$T/args" && grep -q "xrctl" "$T/args" && ! grep -q "xrshare" "$T/args"'
chk '[ -f "$FUSE" ]'
sleep 10
chk '[ "$(cat "$RUN/state")" = stable ] && [ -f "$RUN/ready_at" ] && [ ! -f "$FUSE" ] && [ "$(cat "$LAST_GOOD")" = "$(sh "$T/mod/scripts/xr" internal config-hash)" ]'
chk '$VM status | grep -q "\"running\":true.*\"state\":\"stable\".*\"last_good_match\":true.*\"ready_channel\":\"hvc\""'
$VM start; chk 'grep -q "已在运行" "$LOG"'
# 磁盘操作在运行中被拒绝
$VM expand 2048; chk '[ $? -ne 0 ] && [ "$(stat -c %s "$ROOTFS")" -eq $((300*1048576)) ]'
# 2. 停止
$VM stop
chk '! sh "$T/mod/scripts/xr" running && [ ! -f "$RUN/crosvm.pid" ] && [ ! -f "$RUN/ready_at" ] && [ ! -e "$RUN/clash-ctl.sock" ] && [ ! -e "$RUN/clash_mux.pid" ] && grep -q "net down" "$T/net.calls"'
# 2b. OpenVPN 持久开关：开机按 openvpn.enabled 下发 enable / disable
mkdir -p "$XR_DATA/config" "$RUN/guest/host"
printf 'client\n' > "$XR_DATA/config/openvpn.ovpn"
echo 0 > "$XR_DATA/config/openvpn.enabled"
$VM start; $VM stop
chk 'grep -q "remote\|^client\|." "$RUN/guest/host/openvpn.ovpn" && grep -qx disable "$RUN/guest/host/openvpn.cmd"'
echo 1 > "$XR_DATA/config/openvpn.enabled"
$VM start; $VM stop
chk 'grep -qx enable "$RUN/guest/host/openvpn.cmd"'
# 3. 未就绪阶段：booting 期间 uptime_s 保持为 0 且状态行统一为「启动中」，超时后自动停止并删除熔断文件
FAKE_NO_READY=1 $VM start
chk '$VM status | grep -q "\"running\":true.*\"state\":\"booting\".*\"uptime_s\":0"'
chk 'sh "$T/mod/scripts/xr" status-line | grep -q "^启动中 / 启动中 /"'
sleep 10
chk '[ ! -f "$RUN/crosvm.pid" ] && [ ! -f "$FUSE" ] && grep -q "秒内未就绪" "$LOG"'
chk '$VM status | grep -q "\"running\":false.*\"error\":\"[0-9]* 秒内未就绪\""'   # WebUI 显示失败原因
# 4. crosvm 立即退出
FAKE_EXIT=1 FAKE_NO_READY=1 $VM start; chk '[ $? -ne 0 ] && [ ! -f "$FUSE" ] && grep -q "立即退出" "$LOG"'
chk '[ "$(cat "$RUN/last_error")" = 无法启动 ]'
# 5. 熔断未确认时拒绝启动
date > "$TRIPPED"; $VM start; chk '[ $? -ne 0 ] && grep -q "熔断未确认" "$LOG"'; rm -f "$TRIPPED"
# 6. VM_NET=0 / VM_DISK_RW=0
sed -i 's/^VM_NET=.*/VM_NET="0"/; s/^VM_DISK_RW=.*/VM_DISK_RW="0"/' "$CONF"; : > "$T/net.calls"
$VM start; chk '! grep -q -- "--net" "$T/args" && grep -q "ro=true" "$T/args" && grep -q "root=/dev/vda ro " "$T/args"'
$VM stop
# 7. 扩容 / 重置
$VM expand 100; chk '[ $? -ne 0 ]'
$VM expand 2048; chk '[ "$(stat -c %s "$ROOTFS")" -eq $((2048*1048576)) ] && grep -q "VM_DISK_SIZE_MB=\"2048\"" "$CONF"'
echo x > "$LAST_GOOD"; $VM reset-disk
chk '[ "$(stat -c %s "$ROOTFS")" -eq $((2048*1048576)) ] && [ ! -f "$LAST_GOOD" ]'

[ "$fail" = 0 ] && echo "test-vm OK"
exit "$fail"
