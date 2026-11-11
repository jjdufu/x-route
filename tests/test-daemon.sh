#!/bin/sh
# shellcheck disable=SC2016,SC2317,SC2034
# XRouter: 守护进程与 WebUI 解绑（setsid、命令通道、启动后父进程退出虚拟机仍在）
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
T="$(mktemp -d)"
fail=0; chk() { if eval "$1"; then :; else echo "FAIL: $1"; fail=1; fi; }
mkdir -p "$T/bin" "$T/mod"
cp -a "$ROOT/module/." "$T/mod/"
printf '#!/bin/sh\ncase "$1" in sys.boot_completed) echo 1;; sys.user.0.ce_available) echo true;; esac\n' > "$T/bin/getprop"
chmod +x "$T/bin/getprop"
printf '#!/bin/sh\necho "net $*" >> "%s/net.calls"\n' "$T" > "$T/mod/scripts/net.sh"
export PATH="$T/bin:$PATH" XR_DATA="$T/data" MODDIR="$T/mod" CROSVM="$ROOT/tests/mock/crosvm" FAKE_ARGS="$T/args"
# 本测试走真实守护进程路径，不要前台模式；看门狗拉长间隔以免测试中把进程拉起来
unset XR_DAEMON_FOREGROUND
export XR_WATCHDOG_INTERVAL=300
XR="sh $T/mod/scripts/xr"
cleanup() {
  if [ -d "${XR_DATA:-$T/data}" ]; then
    $XR stop >/dev/null 2>&1
    for f in daemon.pid watchdog.pid; do
      p="$(cat "${XR_DATA}/run/$f" 2>/dev/null)"
      [ -n "$p" ] && kill "$p" 2>/dev/null
    done
    MODDIR="$T/mod" sh "$T/mod/uninstall.sh" >/dev/null 2>&1
  fi
  rm -rf "$T"
}
trap cleanup EXIT
. "$T/mod/scripts/common.sh"; ensure_dirs
cp "$T/mod/config/config.conf" "$CONF"
sed -i 's/^READY_TIMEOUT_SEC=.*/READY_TIMEOUT_SEC="6"/; s/^STABLE_SEC=.*/STABLE_SEC="5"/' "$CONF"
echo k > "$IMG/Image"; truncate -s 300M "$ROOTFS"

# 1. 由即将退出的父进程启动（模拟 ksu.exec 生命周期结束）
sh -c "$XR start; echo \$? > '$T/start.rc'" 
chk '[ "$(cat "$T/start.rc")" = 0 ]'
chk '[ -f "$RUN/daemon.pid" ] && [ -f "$RUN/daemon.ready" ]'
dpid="$(cat "$RUN/daemon.pid")"
cpid="$(cat "$RUN/crosvm.pid")"
chk 'kill -0 "$dpid" 2>/dev/null && kill -0 "$cpid" 2>/dev/null'
# crosvm / 守护进程都不应还在已退出的启动 shell 的进程组里
# 守护进程 cmdline 应含 daemon.sh
chk 'tr "\0" " " < "/proc/$dpid/cmdline" | grep -q daemon.sh'
# 测试进程不是 crosvm 的父进程
ppid="$(awk "{print \$4}" /proc/"$cpid"/stat)"
chk '[ "$ppid" != "$$" ]'
# 启动方已退出后，两者仍在
chk '$XR running'

# 2. 再次 start 视为已在运行
chk '$XR start'

# 3. stop 经守护进程，虚拟机结束；守护进程仍可留存以便再次启动
$XR stop
chk '! $XR running && [ ! -f "$RUN/crosvm.pid" ]'
chk 'kill -0 "$dpid" 2>/dev/null'

# 4. 看门狗已在开机/ensure 路径拉起；再启动后模拟 crosvm 意外退出，须立刻撤路由并记原因
chk '[ -f "$RUN/watchdog.pid" ] && kill -0 "$(cat "$RUN/watchdog.pid")" 2>/dev/null'
chk '$XR start'
: > "$T/net.calls"
cpid="$(cat "$RUN/crosvm.pid")"
kill "$cpid" 2>/dev/null
# 等待守护进程 5 秒循环扫到
w=0; while [ "$w" -lt 12 ]; do grep -q "异常退出" "$LOG" && break; sleep 1; w=$((w+1)); done
chk 'grep -q "异常退出" "$LOG" && grep -q "net down" "$T/net.calls"'
chk '[ -f "$RUN/last_error" ]'

# 5. 卸载结束守护进程并取消通知（mock 中 cmd 可能不存在）
echo still > "$T/data/log/module.log"
MODDIR="$T/mod" sh "$T/mod/uninstall.sh"
chk '[ ! -d "$XR_DATA" ]'
chk '! kill -0 "$dpid" 2>/dev/null'

[ "$fail" = 0 ] && echo "test-daemon OK"
exit "$fail"
