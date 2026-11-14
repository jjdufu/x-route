#!/bin/sh
# shellcheck disable=SC2016,SC2317,SC2034
# XRouter: 保活辅助与看门狗回归测试（cgroup 脱离由 cargo test 覆盖；此处测试 watchdog 撤路由与打包规范）
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
T="$(mktemp -d)"
fail=0; chk() { if eval "$1"; then :; else echo "FAIL: $1"; fail=1; fi; }
WPID=""
cleanup() {
  if [ -n "$WPID" ]; then kill "$WPID" 2>/dev/null || true; fi
  _np="$(cat "$T/data/run/notifier.pid" 2>/dev/null)"
  if [ -n "$_np" ]; then kill "$_np" 2>/dev/null || true; fi
  rm -rf "$T"
}
trap cleanup EXIT

mkdir -p "$T/bin" "$T/cg/system" "$T/proc/1234" "$T/sysnet/xr0" "$T/mod"
cp -a "$ROOT/module/." "$T/mod/"
export PATH="$T/bin:$PATH" XR_DATA="$T/data" MODDIR="$T/mod" \
  XR_CGROUP_ROOT="$T/cg" XR_PROC_BASE="$T/proc" XR_SYSNET="$T/sysnet" \
  XR_WATCHDOG_INTERVAL=1

printf '#!/bin/sh\necho "dumpsys $*" >> "%s/ds.log"\n' "$T" > "$T/bin/dumpsys"
printf '#!/bin/sh\ncase "$1" in sys.boot_completed) echo 1;; sys.user.0.ce_available) echo true;; esac\n' > "$T/bin/getprop"
chmod +x "$T/bin/"*

# 假 net.sh：记录 down
printf '#!/bin/sh\necho "net $*" >> "%s/net.calls"\n' "$T" > "$T/mod/scripts/net.sh"
chmod +x "$T/mod/scripts/net.sh"

RUN="$XR_DATA/run"; LOG="$XR_DATA/log/module.log"
mkdir -p "$XR_DATA/config" "$RUN" "$XR_DATA/log"
cp "$T/mod/config/config.conf" "$XR_DATA/config/config.conf"

# 1. 看门狗检测到残留网关（crosvm 已死但 net.gw 还在）-> 自动撤路由并记异常退出
echo block > "$RUN/net.gw"
echo 999999 > "$RUN/crosvm.pid"
"$T/mod/bin/xr" internal watchdog >/dev/null 2>&1 &
WPID=$!
i=0; while ! grep -q "net down" "$T/net.calls" 2>/dev/null && [ $i -lt 30 ]; do sleep 0.2; i=$((i+1)); done
kill "$WPID" 2>/dev/null; WPID=""
chk 'grep -q "net down" "$T/net.calls"'
chk 'grep -q "异常退出" "$LOG"'

# 例 1 的看门狗可能拉起了真守护进程：先停掉，避免干扰后面的例子
_dp="$(cat "$RUN/daemon.pid" 2>/dev/null)"; if [ -n "$_dp" ] && [ "$_dp" != 999999 ]; then kill "$_dp" 2>/dev/null || true; fi
sleep 0.5

# 准备"crosvm 活着但 xr0 无载波"：假 crosvm 进程 4242、state=stable、net.gw 在；守护进程用一个 sleep 进程冒充
stale_setup() {
  rm -f "$RUN"/* "$XR_DATA/log/vmm.log"; : > "$LOG"
  mkdir -p "$T/proc/4242"; printf 'crosvm\0' > "$T/proc/4242/cmdline"
  echo 4242 > "$RUN/crosvm.pid"; echo stable > "$RUN/state"; echo block > "$RUN/net.gw"; echo 0 > "$T/sysnet/xr0/carrier"
  sleep 300 & SPID=$!
  mkdir -p "$T/proc/$SPID"; printf 'xr\0internal\0daemon\0' > "$T/proc/$SPID/cmdline"; echo "$SPID" > "$RUN/daemon.pid"
}
# 3. 客机复位竞态：vmm.log 有复位标记，crosvm 3 秒后才退出 -> 不报异常退出，crosvm 退出后交给守护进程（SIGUSR2）
stale_setup
printf '%s\n' "=== xr crosvm start 1" "vcpu requested reset" > "$XR_DATA/log/vmm.log"
XR_DAEMON_FOREGROUND=1 "$T/mod/bin/xr" internal watchdog >/dev/null 2>&1 &
WPID=$!
sleep 3; rm -rf "$T/proc/4242"
i=0; while kill -0 "$SPID" 2>/dev/null && [ $i -lt 30 ]; do sleep 0.2; i=$((i+1)); done
kill "$WPID" 2>/dev/null; WPID=""
chk '! grep -q "异常退出" "$LOG"'
chk '! kill -0 "$SPID" 2>/dev/null'   # 冒充守护进程的 sleep 收到 SIGUSR2 退出
kill "$SPID" 2>/dev/null; rm -rf "$T/proc/$SPID"

# 4. 持续无载波（无复位标记，宽限缩短为 2 秒）-> 约 2–4 秒后记一次异常退出（crosvm 仍在运行），不撤路由
stale_setup
printf '%s\n' "=== xr crosvm start 1" > "$XR_DATA/log/vmm.log"
: > "$T/net.calls"
XR_DAEMON_FOREGROUND=1 XR_CARRIER_GRACE=2 "$T/mod/bin/xr" internal watchdog >/dev/null 2>&1 &
WPID=$!
sleep 1.5; chk '! grep -q "异常退出" "$LOG"'   # 宽限期内不报
i=0; while ! grep -q "异常退出" "$LOG" && [ $i -lt 40 ]; do sleep 0.1; i=$((i+1)); done
sleep 2
kill "$WPID" 2>/dev/null; WPID=""
chk '[ "$(grep -c "异常退出：crosvm 仍在运行（看门狗）" "$LOG")" -eq 1 ]'
chk '! grep -q "net down" "$T/net.calls"'
kill "$SPID" 2>/dev/null; rm -rf "$T/proc/$SPID" "$T/proc/4242"

# 2. 打包与工作流规范：移除旧 companion APK 痕迹，保留 notifier/build.sh
chk '! grep -qE "companion|keepalive\.apk|aapt|apksigner" "$ROOT/scripts/package-module.sh" "$ROOT/.github/workflows/package-module.yml" "$ROOT/.github/workflows/build-openwrt.yml"'
chk 'grep -q "notifier/build.sh" "$ROOT/scripts/package-module.sh"'

[ "$fail" = 0 ] && echo "test-keepalive OK"
exit "$fail"
