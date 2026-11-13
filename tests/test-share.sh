#!/bin/sh
# shellcheck disable=SC2016,SC2317,SC2034
# XRouter: share.rs 测试（经 vm.sh start -> bin/xr；ro 包装脚本在用户命名空间里真实执行一次）
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
T="$(mktemp -d)"; trap 'sh "$T/mod/scripts/vm.sh" stop >/dev/null 2>&1; rm -rf "$T"' EXIT
fail=0; chk() { if eval "$1"; then :; else echo "FAIL: $1"; fail=1; fi; }
mkdir -p "$T/bin" "$T/media/Download" "$T/mod"
cp -a "$ROOT/module/." "$T/mod/"
printf '#!/bin/sh\nexit 0\n' > "$T/mod/scripts/net.sh"; chmod +x "$T/mod/scripts/net.sh"
printf '#!/bin/sh\ncase "$1" in sys.boot_completed) echo 1;; sys.user.0.ce_available) cat "%s/ce";; esac\n' "$T" > "$T/bin/getprop"; chmod +x "$T/bin/getprop"
echo true > "$T/ce"
BBX="$(command -v busybox || true)"
BB_MOCK="$T/bin/busybox"
cat > "$BB_MOCK" <<'EOF'
#!/bin/sh
if [ "${1:-}" = unshare ] && [ "${2:-}" = -m ] && [ "${3:-}" = sh ]; then
  shift 4
fi
exec "$@"
EOF
chmod +x "$BB_MOCK"
export PATH="$T/bin:$PATH" XR_DATA="$T/data" MODDIR="$T/mod" XR_MEDIA="$T/media" BB="$BB_MOCK" CROSVM="$ROOT/tests/mock/crosvm" FAKE_ARGS="$T/args" XR_DAEMON_FOREGROUND=1
CONF="$XR_DATA/config/config.conf"; IMG="$XR_DATA/images"; ROOTFS="$IMG/openwrt-rootfs.img"; RUN="$XR_DATA/run"; LOG="$XR_DATA/log/module.log"
mkdir -p "$XR_DATA/config" "$IMG" "$RUN" "$XR_DATA/log"
cp "$T/mod/config/config.conf" "$CONF"
sed -i 's/^READY_TIMEOUT_SEC=.*/READY_TIMEOUT_SEC="6"/; s/^STABLE_SEC=.*/STABLE_SEC="5"/' "$CONF"
echo k > "$IMG/Image"; truncate -s 300M "$ROOTFS"
XR="sh $T/mod/scripts/xr"
VM="sh $T/mod/scripts/vm.sh"

# 1. VM_SHARE=0
$XR internal set-kv VM_SHARE 0 >/dev/null
$VM start >/dev/null 2>&1; chk 'grep -q "xr.share=off" "$T/args" && ! grep -q "xrshare" "$T/args"'
$VM stop >/dev/null 2>&1

# 2. CE 不可用 -> 回退 off
$XR internal set-kv VM_SHARE 1 >/dev/null
$XR internal set-kv VM_SHARE_MODE ro >/dev/null
$XR internal set-kv VM_SHARE_PATH "$T/media" >/dev/null
echo false > "$T/ce"
$VM start >/dev/null 2>&1; chk 'grep -q "xr.share=off" "$T/args" && grep -q "CE 不可用" "$LOG"'
$VM stop >/dev/null 2>&1
echo true > "$T/ce"

# 3. 非法路径（仅测试合法媒体根目录外的拒绝：直接写入 config.conf 模拟手改/异常配置）
nl_dir="$(printf '%s/bad\nx' "$T/media")"; mkdir -p "$nl_dir"
for bad in /data/local/tmp "$T/media/../x" "$T/media/a:b" "$T/media/nope" "$T/media/a,b"; do
  $XR internal set-kv VM_SHARE_PATH "$bad" >/dev/null 2>&1 || sed -i "s|^VM_SHARE_PATH=.*|VM_SHARE_PATH=\"$bad\"|" "$CONF"
  $VM start >/dev/null 2>&1; chk 'grep -q "xr.share=off" "$T/args"'
  $VM stop >/dev/null 2>&1
done
chk '[ "$(grep -c "路径不允许" "$LOG")" -ge 5 ]'

# 4. ro 共享
sed -i "s|^VM_SHARE_PATH=.*|VM_SHARE_PATH=\"$T/media/Download\"|" "$CONF"
$VM start >/dev/null 2>&1
chk 'grep -q "xr.share=ro" "$T/args" && grep -q "$T/media/Download:xrshare:type=fs:cache=auto:timeout=1" "$T/args"'
chk '[ -x "$RUN/share-ro.sh" ] && grep -qx "$BB_MOCK mount -o remount,bind,ro,nosuid,nodev,noexec \"$T/media/Download\" || exit 1" "$RUN/share-ro.sh"'
$VM stop >/dev/null 2>&1

# 5. 真实执行包装脚本（用户命名空间代替 root）：命名空间内只读，命名空间外仍可写
if [ -n "$BBX" ] && "$BBX" unshare -rm sh -c "$BBX mount --bind $T/media $T/media && $BBX mount -o remount,bind,ro $T/media" 2>/dev/null; then
  out="$("$BBX" unshare -rm sh -c "$BBX mount --bind $T/media/Download $T/media/Download && sh $RUN/share-ro.sh && { touch $T/media/Download/w 2>/dev/null && echo WRITABLE || echo RO; }")"
  chk '[ "$out" = RO ]'
  if touch "$T/media/Download/w" 2>/dev/null; then rm -f "$T/media/Download/w"; else echo "FAIL: 外层应仍可写"; fail=1; fi
else
  echo "test-share: unshare 不可用，跳过真实只读验证"
fi

[ "$fail" = 0 ] && echo "test-share OK"
exit "$fail"
