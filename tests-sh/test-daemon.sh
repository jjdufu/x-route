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
export PATH="$T/bin:$PATH" XR_DATA="$T/data" MODDIR="$T/mod" CROSVM="$ROOT/tests-sh/mock/crosvm" FAKE_ARGS="$T/args"
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
CONF="$XR_DATA/config/config.conf"; IMG="$XR_DATA/images"; ROOTFS="$IMG/openwrt-rootfs.img"; RUN="$XR_DATA/run"; LOG="$XR_DATA/log/module.log"
mkdir -p "$XR_DATA/config" "$IMG" "$RUN" "$XR_DATA/log"
is_pid_running() { [ -n "${1:-}" ] && kill -0 "$1" 2>/dev/null && ! grep -q '^[^)]*) Z ' "/proc/$1/stat" 2>/dev/null; }
cp "$T/mod/config/config.conf" "$CONF"
sed -i 's/^READY_TIMEOUT_SEC=.*/READY_TIMEOUT_SEC="6"/; s/^STABLE_SEC=.*/STABLE_SEC="5"/' "$CONF"
echo k > "$IMG/Image"; truncate -s 300M "$ROOTFS"

# 1. 由即将退出的父进程启动（模拟 ksu.exec 生命周期结束）
sh -c "$XR start; echo \$? > '$T/start.rc'" 
chk '[ "$(cat "$T/start.rc")" = 0 ]'
chk '[ -f "$RUN/daemon.pid" ] && [ -f "$RUN/daemon.ready" ]'
dpid="$(cat "$RUN/daemon.pid")"
cpid="$(cat "$RUN/crosvm.pid")"
chk 'is_pid_running "$dpid" && is_pid_running "$cpid"'
# crosvm / 守护进程都不应还在已退出的启动 shell 的进程组里
# 守护进程 cmdline 应含 internal daemon
chk 'tr "\0" " " < "/proc/$dpid/cmdline" | grep -q "internal daemon"'
# 测试进程不是 crosvm 的父进程
ppid="$(awk "{print \$4}" /proc/"$cpid"/stat)"
chk '[ "$ppid" != "$$" ]'
# 启动方已退出后，两者仍在
chk '$XR running'

# 2. 再次 start 视为已在运行
chk '$XR start'

# 3. stop 经守护进程，虚拟机结束；守护进程仍可留存以便再次启动，且 fuse_watch 退出不得误删 daemon.pid 或写 last_error
$XR stop
chk '! $XR running && [ ! -f "$RUN/crosvm.pid" ] && [ ! -f "$RUN/stopping" ] && [ ! -f "$RUN/last_error" ]'
sleep 2
chk 'is_pid_running "$dpid" && [ "$(cat "$RUN/daemon.pid" 2>/dev/null)" = "$dpid" ]'

# 4. 看门狗已在开机/ensure 路径拉起；再启动后模拟 crosvm 意外退出，须立刻撤路由并记原因
chk '[ -f "$RUN/watchdog.pid" ] && is_pid_running "$(cat "$RUN/watchdog.pid")"'
chk '$XR start'
: > "$T/net.calls"
cpid="$(cat "$RUN/crosvm.pid")"
kill "$cpid" 2>/dev/null
# 等待守护进程 5 秒循环扫到
w=0; while [ "$w" -lt 15 ]; do grep -q "异常退出" "$LOG" && grep -q "net down" "$T/net.calls" && [ -f "$RUN/last_error" ] && break; sleep 1; w=$((w+1)); done
chk 'grep -q "异常退出" "$LOG" && grep -q "net down" "$T/net.calls"'
chk '[ -f "$RUN/last_error" ]'

# 4a. 客机内部请求重启（如 LuCI 点击重启 -> vcpu requested reset）：守护进程应自动重拉 crosvm，不记异常退出、不留僵尸进程
rm -f "$RUN/last_error"
chk '$XR start'
old_cpid="$(cat "$RUN/crosvm.pid")"
n_err="$(grep -c "异常退出" "$LOG")"
touch "$RUN/crosvm.sock.reset"
w=0
while [ "$w" -lt 20 ]; do
  new_cpid="$(cat "$RUN/crosvm.pid" 2>/dev/null || true)"
  if [ -n "$new_cpid" ] && [ "$new_cpid" != "$old_cpid" ] && is_pid_running "$new_cpid" && $XR running; then
    break
  fi
  sleep 0.5
  w=$((w+1))
done
new_cpid="$(cat "$RUN/crosvm.pid" 2>/dev/null || true)"
chk '[ -n "$new_cpid" ] && [ "$new_cpid" != "$old_cpid" ] && is_pid_running "$new_cpid" && $XR running'
chk '[ ! -d "/proc/$old_cpid" ]'
chk '[ "$(grep -c "异常退出" "$LOG")" = "$n_err" ] && [ ! -f "$RUN/last_error" ]'
chk 'grep -q "检测到 OpenWrt 重启请求" "$LOG"'

# 4b. 系统关机：crosvm 被 init 一起结束不算崩溃——不写 last_error、不记「异常退出」、只记一行 INFO，守护进程安静退出
rm -f "$RUN/last_error"
$XR stop
chk '$XR start'
dpid="$(cat "$RUN/daemon.pid")"
n0="$(grep -c "异常退出" "$LOG")"
printf '#!/bin/sh\ncase "$1" in sys.boot_completed) echo 1;; sys.user.0.ce_available) echo true;; sys.powerctl) echo reboot,userrequested;; esac\n' > "$T/bin/getprop"
kill "$(cat "$RUN/crosvm.pid")" 2>/dev/null
w=0; while is_pid_running "$dpid" && [ "$w" -lt 15 ]; do sleep 1; w=$((w+1)); done
chk '! is_pid_running "$dpid"'
chk '[ "$(grep -c "异常退出" "$LOG")" = "$n0" ] && [ ! -f "$RUN/last_error" ]'
chk '[ "$(grep -c "系统关机，服务随之停止" "$LOG")" = 1 ]'
chk '! grep -v "^[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9] [0-9:]* \[" "$LOG" | grep -q .'
printf '#!/bin/sh\ncase "$1" in sys.boot_completed) echo 1;; sys.user.0.ce_available) echo true;; esac\n' > "$T/bin/getprop"

# 4c. 守护进程状态机与 quit 命令：就绪后过 STABLE_SEC 变 stable 并写 last_good；
#     quit（daemon.cmd + USR1）先停虚拟机、做一次流量快照，再回 daemon.res=0 并删除 daemon.pid
MOCK="$T/mock"; mkdir -p "$MOCK"; : > "$MOCK/chain.XR_ACCT_UP"; : > "$MOCK/chain.XR_ACCT_DN"
export MOCK XR_SYSBIN="$ROOT/tests-sh/mock/net"
rm -f "$XR_DATA/config/last_good" "$RUN/shutdown" "$RUN/shutdown.noted"   # 4b 的关机标记
chk '$XR start'
dpid="$(cat "$RUN/daemon.pid")"
w=0; while [ "$(cat "$RUN/state" 2>/dev/null)" != stable ] && [ "$w" -lt 20 ]; do sleep 1; w=$((w+1)); done
chk '[ "$(cat "$RUN/state")" = stable ] && [ -s "$XR_DATA/config/last_good" ]'
: > "$MOCK/calls"; rm -f "$RUN/daemon.res"
echo quit > "$RUN/daemon.cmd"; kill -USR1 "$dpid"
w=0; while [ ! -f "$RUN/daemon.res" ] && [ "$w" -lt 30 ]; do sleep 1; w=$((w+1)); done
chk '[ "$(cat "$RUN/daemon.res")" = 0 ]'
w=0; while is_pid_running "$dpid" && [ "$w" -lt 10 ]; do sleep 1; w=$((w+1)); done
chk '[ ! -f "$RUN/daemon.pid" ] && ! is_pid_running "$dpid" && ! $XR running'
chk 'grep -q -- "-nvxL XR_ACCT_UP" "$MOCK/calls"'
chk '[ ! -e "$RUN/stats.sock" ]'   # 退出时删除自己的速率套接字

# 4d. 实时速率（需求 7）：守护进程只在收到请求时采样计数链、在内存中算速率；
#     首次为 null，2 秒后按增量出速率（不写任何文件）；守护进程不在时 status-json 降级 source=none，1 秒内返回
J() { python3 -c "import json,sys; d=json.load(sys.stdin); print($1)"; }
printf '%s\n' "-d 10.0.0.0/8 -j RETURN" "-j RETURN" > "$MOCK/chain.XR_ACCT_UP"
printf '%s\n' "-s 10.0.0.0/8 -j RETURN" "-j RETURN" > "$MOCK/chain.XR_ACCT_DN"
printf '%s\n' 1000 5000 > "$MOCK/cnt.XR_ACCT_UP"; printf '%s\n' 2000 9000 > "$MOCK/cnt.XR_ACCT_DN"
chk '$XR start'
dpid="$(cat "$RUN/daemon.pid")"
w=0; while [ ! -S "$RUN/stats.sock" ] && [ "$w" -lt 10 ]; do sleep 1; w=$((w+1)); done
chk '[ -S "$RUN/stats.sock" ] && [ "$(stat -c %a "$RUN/stats.sock")" = 600 ]'
ls -l --time-style=+%s "$XR_DATA/config" > "$T/cfg.before" 2>/dev/null
s1="$($XR status-json)"
chk 'echo "$s1" | J "d[\"traffic\"][\"source\"]==\"daemon\" and d[\"traffic\"][\"counting\"] is True and d[\"traffic\"][\"rate\"] is None and d[\"traffic\"][\"total\"][\"total\"]==dict(up=6000,down=11000)" | grep -qx True'
sleep 2
printf '%s\n' 1000 9000 > "$MOCK/cnt.XR_ACCT_UP"
s2="$($XR status-json)"
chk 'echo "$s2" | J "1800<=d[\"traffic\"][\"rate\"][\"total\"][\"up\"]<=2200 and d[\"traffic\"][\"rate\"][\"lan\"][\"up\"]==0 and d[\"traffic\"][\"rate\"][\"total\"][\"down\"]==0" | grep -qx True'
ls -l --time-style=+%s "$XR_DATA/config" > "$T/cfg.after" 2>/dev/null
chk 'cmp -s "$T/cfg.before" "$T/cfg.after"'   # 速率查询不写 config 目录下任何文件
$XR stop >/dev/null 2>&1
kill "$dpid"; w=0; while is_pid_running "$dpid" && [ "$w" -lt 10 ]; do sleep 1; w=$((w+1)); done
chk '! is_pid_running "$dpid" && [ ! -e "$RUN/stats.sock" ]'
out="$(python3 -c 'import subprocess,sys,time; t=time.time(); r=subprocess.run(sys.argv[1:],capture_output=True,text=True); print(r.returncode, int((time.time()-t)*1000)); print(r.stdout)' sh "$T/mod/scripts/xr" status-json)"
chk 'echo "$out" | head -n 1 | { read -r rc ms; [ "$rc" = 0 ] && [ "$ms" -le 1000 ]; }'
chk 'echo "$out" | tail -n 1 | J "d[\"traffic\"][\"source\"]==\"none\" and d[\"traffic\"][\"rate\"] is None" | grep -qx True'
unset XR_SYSBIN

# 5. 卸载结束守护进程并取消通知（mock 中 cmd 可能不存在）
echo still > "$T/data/log/module.log"
MODDIR="$T/mod" sh "$T/mod/uninstall.sh"
chk '[ ! -d "$XR_DATA" ]'
chk '! is_pid_running "$dpid"'

[ "$fail" = 0 ] && echo "test-daemon OK"
exit "$fail"
