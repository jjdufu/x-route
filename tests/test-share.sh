#!/bin/sh
# shellcheck disable=SC2016,SC2317,SC2034
# XRouter: share.sh 测试（假 getprop；ro 包装脚本在用户命名空间里真实执行一次）
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT
fail=0; chk() { if eval "$1"; then :; else echo "FAIL: $1"; fail=1; fi; }
mkdir -p "$T/bin" "$T/media/Download"
CE=true; printf '#!/bin/sh\n[ "$1" = sys.user.0.ce_available ] && cat "%s/ce"\n' "$T" > "$T/bin/getprop"; chmod +x "$T/bin/getprop"
echo true > "$T/ce"
BBX="$(command -v busybox)"
export PATH="$T/bin:$PATH" XR_DATA="$T/data" XR_MEDIA="$T/media" BB="$BBX"
. "$ROOT/module/scripts/common.sh"; ensure_dirs
. "$ROOT/module/scripts/share.sh"
VM_SHARE=0; VM_SHARE_MODE=ro; VM_SHARE_PATH="$T/media"
share_prepare; chk '[ "$share_mode" = off ] && [ -z "$share_spec" ] && [ -z "$share_wrap" ]'
VM_SHARE=1
echo false > "$T/ce"; share_prepare; chk '[ "$share_mode" = off ] && grep -q "CE 不可用" "$LOG"'
echo true > "$T/ce"
for bad in /data/local/tmp "$T/media/../x" "$T/media/a:b" "$T/media/nope" "$T/media/a,b"; do
  VM_SHARE_PATH="$bad"; share_prepare; chk '[ "$share_mode" = off ] && [ -z "$share_spec" ]'
done
chk 'grep -c "路径不允许" "$LOG" | grep -qx 5'
# ro
VM_SHARE_PATH="$T/media/Download"; share_prepare
chk '[ "$share_mode" = ro ] && [ "$share_spec" = "$T/media/Download:xrshare:type=fs:cache=auto:timeout=1" ]'
chk '[ "$share_wrap" = "$BBX unshare -m sh $RUN/share-ro.sh" ] && [ -x "$RUN/share-ro.sh" ]'
chk 'grep -qx "$BBX mount -o remount,bind,ro,nosuid,nodev,noexec \"$T/media/Download\" || exit 1" "$RUN/share-ro.sh"'
# 真实执行包装脚本（用户命名空间代替 root）：命名空间内只读，命名空间外仍可写
if "$BBX" unshare -rm sh -c "$BBX mount --bind $T/media $T/media && $BBX mount -o remount,bind,ro $T/media" 2>/dev/null; then
  "$BBX" unshare -rm sh "$RUN/share-ro.sh" sh -c "touch '$T/media/Download/x'" 2>"$T/err"
  chk '[ ! -e "$T/media/Download/x" ] && grep -q "Read-only" "$T/err"'
  "$BBX" unshare -rm sh "$RUN/share-ro.sh" cat /proc/self/mountinfo > "$T/mi"
  chk 'grep -q " $T/media/Download .*ro,nosuid,nodev,noexec" "$T/mi"'
  chk 'touch "$T/media/Download/y" && ! grep -q "$T/media/Download" /proc/self/mountinfo'
else
  echo "skip: 无法创建用户命名空间，跳过 ro 实际挂载测试"
fi
# rw：RW_MAP_OPT 为空 -> 不共享；非空 -> 带映射参数、无包装
VM_SHARE_MODE=rw; RW_MAP_OPT=""; share_prepare
chk '[ "$share_mode" = off ] && grep -q "rw 不可用" "$LOG" && ! valid_kv VM_SHARE_MODE rw'
RW_MAP_OPT="uidmap=0 1023 1:gidmap=0 1023 1"; share_prepare
chk '[ "$share_mode" = rw ] && [ "$share_spec" = "$T/media/Download:xrshare:type=fs:cache=auto:timeout=1:uidmap=0 1023 1:gidmap=0 1023 1" ] && [ -z "$share_wrap" ]'
chk 'valid_kv VM_SHARE_MODE rw'
# /sdcard 按同一处存储挂到 MEDIA
VM_SHARE=1; VM_SHARE_MODE=ro; VM_SHARE_PATH=/sdcard; share_prepare
chk '[ "$share_mode" = ro ] && [ "$share_spec" = "$T/media:xrshare:type=fs:cache=auto:timeout=1" ]'
VM_SHARE_PATH=/storage/emulated/0/Download; share_prepare
chk '[ "$share_mode" = ro ] && [ "$share_spec" = "$T/media/Download:xrshare:type=fs:cache=auto:timeout=1" ]'
if [ "$fail" = 0 ]; then echo "test-share OK"; else echo "test-share FAILED"; exit 1; fi
