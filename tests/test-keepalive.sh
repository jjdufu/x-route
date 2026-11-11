#!/bin/sh
# shellcheck disable=SC2016,SC2317,SC2034
# XRouter: 保活辅助（cgroup 脱离、异常退出撤路由、LSPosed 式常驻通知进程）
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
T="$(mktemp -d)"
fail=0; chk() { if eval "$1"; then :; else echo "FAIL: $1"; fail=1; fi; }
cleanup() {
  _np="$(cat "$T/data/run/notifier.pid" 2>/dev/null)"; [ -n "$_np" ] && kill "$_np" 2>/dev/null
  rm -rf "$T"
}
trap cleanup EXIT

mkdir -p "$T/bin" "$T/cg/system" "$T/proc/1234" "$T/sysnet/xr0"
export PATH="$T/bin:$PATH" XR_DATA="$T/data" MODDIR="$T/mod" \
  XR_CGROUP_ROOT="$T/cg" XR_PROC_BASE="$T/proc" XR_SYSNET="$T/sysnet"
cp -a "$ROOT/module/." "$T/mod/"
# 假外部命令。app_process 模拟通知进程：serve 时按 notify.req 回执 notify.ack，收到 cancel 撤销并退出
printf '#!/bin/sh\necho "dumpsys $*" >> "%s/ds.log"\n' "$T" > "$T/bin/dumpsys"
cat > "$T/bin/app_process" <<'APP'
#!/bin/sh
echo "app_process $*" >> "$XR_TEST_DIR/app.log"
last1=""; last2=""
for a in "$@"; do last2="$last1"; last1="$a"; done
mode="$last2"; run="$last1"
[ "$mode" = cancel ] && { echo cancel-once >> "$XR_TEST_DIR/app.log"; exit 0; }
mkdir -p "$XR_PROC_BASE/$$"; printf 'xrnotify\0' > "$XR_PROC_BASE/$$/cmdline"
trap 'rm -rf "$XR_PROC_BASE/$$"; exit 0' TERM
seen=""
while :; do
  l="$(cat "$run/notify.req" 2>/dev/null)"
  if [ -n "$l" ] && [ "$l" != "$seen" ]; then
    seen="$l"
    if [ "$l" = cancel ]; then echo "ok cancel" > "$run/notify.ack"; echo cancelled >> "$XR_TEST_DIR/app.log"; rm -rf "$XR_PROC_BASE/$$"; exit 0; fi
    if [ -f "$XR_TEST_DIR/notify.fail" ]; then echo "err boom" > "$run/notify.ack"; rm -rf "$XR_PROC_BASE/$$"; exit 1; fi
    echo "post $l" >> "$XR_TEST_DIR/app.log"; echo "ok $l" > "$run/notify.ack"
  fi
  sleep 0.1
done
APP
chmod +x "$T/bin/"*
export XR_TEST_DIR="$T" XR_APP_PROCESS="$T/bin/app_process" XR_NOTIFY_WAIT=50
# 假 net.sh：记录 down
mkdir -p "$T/mod/scripts"
# keep real keepalive/common; only stub net.sh
printf '#!/bin/sh\necho "net $*" >> "%s/net.calls"\n' "$T" > "$T/mod/scripts/net.sh"
chmod +x "$T/mod/scripts/net.sh"

. "$T/mod/scripts/common.sh"
. "$T/mod/scripts/keepalive.sh"
ensure_dirs

# 1. cgroup 脱离：从应用 cgroup 写入根 cgroup.procs
echo "0::/apps/uid_10383/pid_16139" > "$T/proc/1234/cgroup"
echo 1234 > "$T/proc/1234/oom_score_adj"
: > "$T/cg/cgroup.procs"
: > "$T/cg/system/cgroup.procs"
# 伪造成仍在运行
# shellcheck disable=SC2329
is_pid_running() { [ "$1" = 1234 ] || [ -d "$T/proc/$1" ]; }
chk 'cgroup_in_apps 1234'
cgroup_escape 1234
chk 'grep -qx 1234 "$T/cg/cgroup.procs"'
chk 'grep -qx 1234 "$T/cg/system/cgroup.procs"'
chk '[ "$(cat "$T/proc/1234/oom_score_adj")" = -1000 ]'
# 脱离后 /proc/cgroup 若已变到根，则不再视为 apps
echo "0::/" > "$T/proc/1234/cgroup"
chk '! cgroup_in_apps 1234'

# 3. 异常退出时立刻撤路由（虚拟机不在）
# vm_running 未定义时 emergency_teardown 应 down
rm -f "$T/net.calls"
emergency_teardown "测试杀死守护进程"
chk 'grep -qx "net down" "$T/net.calls"'
chk 'grep -q "异常退出：测试杀死守护进程" "$LOG"'
chk '[ "$(cat "$RUN/last_error")" = "虚拟机意外退出" ]'

# 4. 虚拟机还在时不撤路由
printf '#!/bin/sh\necho "net $*" >> "%s/net.calls"\n' "$T" > "$T/mod/scripts/net.sh"
: > "$T/net.calls"
vm_running() { return 0; }
emergency_teardown "守护进程退出但虚拟机仍在"
chk '! grep -q "net down" "$T/net.calls"'
unset -f vm_running

# 5. 残留接管：虚拟机已死但 net.gw 还在
echo block > "$RUN/net.gw"
chk 'stale_gateway'
echo 0 > "$T/sysnet/xr0/carrier"
vm_running() { return 0; }
chk 'stale_gateway'   # 无载波
echo 1 > "$T/sysnet/xr0/carrier"
chk '! stale_gateway'
unset -f vm_running
rm -f "$RUN/net.gw"
chk '! stale_gateway'

# 7. 模拟管理器被划掉：父进程组被杀后，写到根 cgroup 的子进程标记仍在
# （本测试不真的 kill -PGID，只验证 escape 目标不是 apps 路径）
echo "0::/apps/com.sukisu.ultra" > "$T/proc/1234/cgroup"
chk 'cgroup_in_apps 1234'
: > "$T/cg/cgroup.procs"
cgroup_escape 1234
chk 'grep -qx 1234 "$T/cg/cgroup.procs"'


# 8. 回归：看门狗未引入 vm.sh（vm_running 未定义）时，crosvm 仍在跑、xr0 有载波，不得撤路由
unset -f vm_running 2>/dev/null
mkdir -p "$T/proc/4321"; printf 'crosvm\0run\0' > "$T/proc/4321/cmdline"
echo 4321 > "$RUN/crosvm.pid"
echo block > "$RUN/net.gw"
echo 1 > "$T/sysnet/xr0/carrier"
: > "$T/net.calls"
chk 'xr_vm_alive'
chk '! stale_gateway'
emergency_teardown "测试"
chk '[ -f "$RUN/crosvm.pid" ]'
chk '! grep -q "net down" "$T/net.calls"'
# crosvm 真退出后才撤
rm -rf "$T/proc/4321"
chk '! xr_vm_alive'
chk 'stale_gateway'
rm -f "$RUN/net.gw" "$RUN/crosvm.pid"
# 看门狗脚本必须引入 vm.sh
chk 'grep -q "scripts/vm.sh" "$T/mod/scripts/watchdog.sh"'

# 9. 通知负载
acct_total() { cat "$T/traffic"; }
echo "100 200 30 40" > "$T/traffic"
echo stable > "$RUN/state"
chk '[ "$(XR_NOTIFY_TRAFFIC=wan notify_payload)" = "运行中 wan 100 200 30 40" ]'
chk '[ "$(XR_NOTIFY_TRAFFIC=bogus notify_payload)" = "运行中 total 100 200 30 40" ]'

# 10. 通知进程：经 app_process 以 lspd 同样的方式拉起，回执 ok 才算发出
chk '[ "$XR_NOTIFIER_JAR" = "$T/mod/bin/xrnotify.jar" ] && [ -s "$XR_NOTIFIER_JAR" ]'
chk '! notifier_running'
: > "$T/app.log"
chk 'notifier_post "运行中 lan 1 2 3 5000000000"'
chk 'grep -qx "app_process -Djava.class.path=$T/mod/bin/xrnotify.jar /system/bin --nice-name=xrnotify com.xrouter.notify.Main serve $RUN" "$T/app.log"'
chk 'grep -qx "post 运行中 lan 1 2 3 5000000000" "$T/app.log"'
chk 'notifier_running'
np="$(cat "$RUN/notifier.pid")"
# 已在跑时不重复拉起
chk 'notifier_post "运行中 lan 1 2 3 5000000001"'
chk '[ "$(cat "$RUN/notifier.pid")" = "$np" ] && [ "$(grep -c "Main serve" "$T/app.log")" = 1 ]'
# cgroup 脱离覆盖通知进程
chk 'grep -q "notifier.pid" "$T/mod/scripts/keepalive.sh"'

# 11. notify_update：节流、内容不变不推、息屏暂停、force 立即刷新
rm -f "$RUN"/notify.on "$RUN"/notify.last "$RUN"/notify.ts; : > "$T/app.log"
mkdir -p "$T/proc/4321"; printf 'crosvm\0run\0' > "$T/proc/4321/cmdline"; echo 4321 > "$RUN/crosvm.pid"
export XR_SCREEN=on
posts() { grep -c "^post " "$T/app.log"; }
chk 'notify_update'
chk '[ "$(posts)" = 1 ] && [ -f "$RUN/notify.on" ] && grep -qx "post 运行中 total 100 200 30 40" "$T/app.log"'
echo "101 200 30 40" > "$T/traffic"
notify_update   # 10 秒内：不推
chk '[ "$(posts)" = 1 ]'
echo 0 > "$RUN/notify.ts"; echo "100 200 30 40" > "$T/traffic"
notify_update   # 到点但内容没变：不推
chk '[ "$(posts)" = 1 ]'
echo 0 > "$RUN/notify.ts"; echo "999 200 30 40" > "$T/traffic"
XR_SCREEN=off notify_update   # 息屏：暂停
chk '[ "$(posts)" = 1 ]'
echo 0 > "$RUN/notify.ts"
notify_update   # 亮屏且变化：推送
chk '[ "$(posts)" = 2 ] && grep -qx "post 运行中 total 999 200 30 40" "$T/app.log"'
: > "$RUN/notify.force"; XR_NOTIFY_TRAFFIC=lan XR_SCREEN=off notify_update   # 改设置：立即刷新，即使息屏
chk '[ "$(posts)" = 3 ] && grep -qx "post 运行中 lan 999 200 30 40" "$T/app.log" && [ ! -f "$RUN/notify.force" ]'
# 不再依赖 dumpsys notification 校验
chk '! grep -q "dumpsys notification" "$T/ds.log" 2>/dev/null'

# 12. 通知进程意外退出：下一轮重新拉起并重发（即使内容没变）
kill "$(cat "$RUN/notifier.pid")"; sleep 1
chk '! notifier_running'
notify_update
chk 'notifier_running && [ "$(cat "$RUN/notifier.pid")" != "$np" ] && [ "$(grep -c "Main serve" "$T/app.log")" = 1 ] && [ "$(posts)" = 4 ]'

# 13. 发送失败：只记一次日志，返回失败
kill "$(cat "$RUN/notifier.pid")"; sleep 1
: > "$T/notify.fail"; : > "$LOG"
: > "$RUN/notify.force"
chk '! notify_update'
: > "$RUN/notify.force"; notify_update
chk '[ "$(grep -c "常驻通知发送失败：err boom" "$LOG")" = 1 ] && [ ! -f "$RUN/notify.on" ]'
rm -f "$T/notify.fail"
: > "$RUN/notify.force"
chk 'notify_update && [ -f "$RUN/notify.on" ] && [ ! -f "$RUN/notify.warned" ]'

# 14. 虚拟机停了：让通知进程撤掉并退出，只撤一次
rm -rf "$T/proc/4321"; : > "$T/app.log"
notify_update; notify_update
chk '[ "$(grep -c "^cancelled" "$T/app.log")" = 1 ] && ! grep -q cancel-once "$T/app.log"'
chk '! notifier_running && [ ! -f "$RUN/notify.on" ] && [ ! -f "$RUN/notifier.pid" ]'

# 15. 通知进程不在时撤销：一次性拉起 cancel（清掉崩溃后留下的通知）
: > "$T/app.log"
notify_cancel
chk 'grep -qx "app_process -Djava.class.path=$T/mod/bin/xrnotify.jar /system/bin --nice-name=xrnotify com.xrouter.notify.Main cancel $RUN" "$T/app.log"'
chk 'grep -qx cancel-once "$T/app.log"'

# 16. 缺少 jar：拉不起，返回失败
mv "$XR_NOTIFIER_JAR" "$T/jar.bak"
chk '! notifier_start'
mv "$T/jar.bak" "$XR_NOTIFIER_JAR"
unset XR_SCREEN
unset -f acct_total

# 17. 伴生 APK 及其回退已彻底移除
chk '! grep -rqE "keepalive\.apk|com\.xrouter\.keepalive|KeepAliveService|open\.req|cmd notification|pm install|doze_whitelist" "$ROOT/module"'


[ "$fail" = 0 ] && echo "test-keepalive OK"
exit "$fail"
