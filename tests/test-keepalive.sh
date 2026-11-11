#!/bin/sh
# shellcheck disable=SC2016,SC2317,SC2034
# XRouter: 保活辅助（cgroup 脱离、通知校验与回退、异常退出撤路由、电池白名单）
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
T="$(mktemp -d)"
fail=0; chk() { if eval "$1"; then :; else echo "FAIL: $1"; fail=1; fi; }
cleanup() { rm -rf "$T"; }
trap cleanup EXIT

mkdir -p "$T/bin" "$T/cg/system" "$T/proc/1234" "$T/sysnet/xr0"
export PATH="$T/bin:$PATH" XR_DATA="$T/data" MODDIR="$T/mod" \
  XR_CGROUP_ROOT="$T/cg" XR_PROC_BASE="$T/proc" XR_SYSNET="$T/sysnet"
cp -a "$ROOT/module/." "$T/mod/"
# 假外部命令
printf '#!/bin/sh\necho "cmd $*" >> "%s/cmd.log"; case "$*" in\n  "notification post --ongoing"*) echo "usage: notification post"; exit 0 ;;\n  "notification post"*) echo "couldn'\''t find notification after enqueueing"; exit 0 ;;\n  "appops set"*|"netpolicy"*) echo "$*" >> "%s/appops.log" ;;\nesac\nexit 0\n' "$T" "$T" > "$T/bin/cmd"
printf '#!/bin/sh\necho "dumpsys $*" >> "%s/ds.log"; [ -f "%s/notify.visible" ] && echo XRouter com.xrouter.keepalive; [ "$1" = deviceidle ] && echo "$*" >> "%s/idle.log"\n' "$T" "$T" "$T" > "$T/bin/dumpsys"
printf '#!/bin/sh\necho "am $*" >> "%s/am.log"; exit 1\n' "$T" > "$T/bin/am"
printf '#!/bin/sh\necho "pm $*" >> "%s/pm.log"; exit 1\n' "$T" > "$T/bin/pm"
chmod +x "$T/bin/"*
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

# 2. 通知：--ongoing 与普通 post 都可能 exit 0 但实际失败，必须走回退且不得当成成功
chk 'notify_cmd_failed "usage: notification post [flags]" 0'
chk 'notify_cmd_failed "couldn'\''t find notification after enqueueing" 0'
chk 'notify_cmd_failed "Error: unknown option" 0'
chk '! notify_cmd_failed "" 0'
chk '! notify_visible'
chk '! notify_cmd_post'
chk 'grep -q "notification post" "$T/cmd.log"'
# 没有 APK 时 notify_apk_start 失败
chk '! notify_apk_start && grep -q "pm path" "$T/pm.log"'
# dumpsys 能看见时，cmd 成功路径才算发出
: > "$T/notify.visible"
# 但 cmd 输出仍是失败文案，仍应拒绝
chk '! notify_cmd_post'

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

# 6. 电池优化 / Doze 白名单
doze_whitelist com.xrouter.keepalive
chk 'grep -q "whitelist +com.xrouter.keepalive" "$T/idle.log"'
chk 'grep -q "RUN_IN_BACKGROUND allow" "$T/appops.log"'
chk 'grep -q "RUN_ANY_IN_BACKGROUND allow" "$T/appops.log"'

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

# 9. 保活应用经标准输入流式安装（system_server 读不到 /data/adb 路径）
printf '#!/bin/sh\necho "pm $*" >> "%s/pm.log"; case "$*" in "path "*) exit 1 ;; *"-S "*) cat >/dev/null; echo Success ;; esac; exit 1\n' "$T" > "$T/bin/pm"
chmod +x "$T/bin/pm"
mkdir -p "$T/mod/apk"; echo apk > "$T/mod/apk/keepalive.apk"
: > "$T/pm.log"
chk 'install_keepalive_apk'
chk 'grep -q "install -r -g -S 4" "$T/pm.log"'

# 10. 伴生应用版本：已装版本低于模块自带版本时要覆盖升级
echo 2 > "$T/mod/apk/keepalive.ver"
printf '#!/bin/sh\necho "pm $*" >> "%s/pm.log"; case "$*" in "path "*) exit 0 ;; "list packages"*) echo "package:com.xrouter.keepalive versionCode:$(cat "%s/apkver")" ;; esac; exit 0\n' "$T" "$T" > "$T/bin/pm"
chmod +x "$T/bin/pm"
echo 1 > "$T/apkver"; chk '! apk_current'
echo 2 > "$T/apkver"; chk 'apk_current'
echo 3 > "$T/apkver"; chk 'apk_current'

# 11. 累计流量推送：负载、显式组件与 extras
acct_total() { cat "$T/traffic"; }
echo "100 200 30 40" > "$T/traffic"
echo stable > "$RUN/state"
chk '[ "$(XR_NOTIFY_TRAFFIC=wan notify_payload)" = "运行中 wan 100 200 30 40" ]'
chk '[ "$(XR_NOTIFY_TRAFFIC=bogus notify_payload)" = "运行中 total 100 200 30 40" ]'
printf '#!/bin/sh\necho "am $*" >> "%s/am.log"; [ -f "%s/am.err" ] && echo "Error: boom"; exit 0\n' "$T" "$T" > "$T/bin/am"
chmod +x "$T/bin/am"
: > "$T/am.log"
chk 'notify_apk_push "运行中 lan 1 2 3 5000000000"'
chk 'grep -qx "am start-foreground-service -n com.xrouter.keepalive/.KeepAliveService --es state 运行中 --es mode lan --el wan_up 1 --el wan_dn 2 --el lan_up 3 --el lan_dn 5000000000" "$T/am.log"'
: > "$T/am.err"; chk '! notify_apk_push "运行中 total 0 0 0 0"'; rm -f "$T/am.err"

# 12. notify_update：节流、内容不变不推、息屏暂停、force 立即刷新、停机只撤一次
mkdir -p "$T/proc/4321"; printf 'crosvm\0run\0' > "$T/proc/4321/cmdline"; echo 4321 > "$RUN/crosvm.pid"
: > "$T/notify.visible"
rm -f "$RUN"/notify.*; : > "$T/am.log"; : > "$T/ds.log"
export XR_SCREEN=on
chk 'notify_update'
chk '[ "$(grep -c start-foreground-service "$T/am.log")" = 1 ] && [ -f "$RUN/notify.on" ]'
chk 'grep -q "POST_NOTIFICATIONS" "$T/pm.log" && grep -q "POST_NOTIFICATION allow" "$T/appops.log"'
echo "101 200 30 40" > "$T/traffic"
notify_update   # 10 秒内：不推
chk '[ "$(grep -c start-foreground-service "$T/am.log")" = 1 ]'
echo 0 > "$RUN/notify.ts"; echo "100 200 30 40" > "$T/traffic"
notify_update   # 到点但内容没变：不推
chk '[ "$(grep -c start-foreground-service "$T/am.log")" = 1 ]'
echo 0 > "$RUN/notify.ts"; echo "999 200 30 40" > "$T/traffic"
XR_SCREEN=off notify_update   # 息屏：暂停
chk '[ "$(grep -c start-foreground-service "$T/am.log")" = 1 ]'
echo 0 > "$RUN/notify.ts"
notify_update   # 亮屏且变化：推送
chk '[ "$(grep -c start-foreground-service "$T/am.log")" = 2 ] && grep -q "wan_up 999" "$T/am.log"'
: > "$RUN/notify.force"; XR_NOTIFY_TRAFFIC=lan XR_SCREEN=off notify_update   # 改设置：立即刷新，即使息屏
chk '[ "$(grep -c start-foreground-service "$T/am.log")" = 3 ] && grep -q "mode lan" "$T/am.log" && [ ! -f "$RUN/notify.force" ]'
chk '[ "$(grep -c "dumpsys notification" "$T/ds.log")" = 1 ]'
rm -rf "$T/proc/4321"; : > "$T/am.log"
notify_update; notify_update
chk '[ "$(grep -c stopservice "$T/am.log")" = 1 ] && [ ! -f "$RUN/notify.on" ]'
unset XR_SCREEN
unset -f acct_total

# 13. 通知点击：root 打开管理器的 WebUIActivity（未导出，普通应用打不开）
export XR_KEEPALIVE_DIR="$T/kadir"; mkdir -p "$XR_KEEPALIVE_DIR"
printf '#!/bin/sh\necho "pm $*" >> "%s/pm.log"; case "$*" in "path com.sukisu.ultra") exit 0 ;; "path "*) exit 1 ;; esac; exit 0\n' "$T" > "$T/bin/pm"
chmod +x "$T/bin/pm"
chk '! open_req_pending'
echo 1 > "$XR_KEEPALIVE_DIR/open.req"
chk 'open_req_pending'
: > "$T/am.log"
chk 'open_webui'
chk 'grep -qx "am start -n com.sukisu.ultra/com.sukisu.ultra.ui.webui.WebUIActivity -d ksu://webui?id=xrouter --es id xrouter --es name XRouter" "$T/am.log"'
chk '! open_req_pending'
# 活动类不存在时依次回退，最后打开管理器首页
: > "$T/am.err"; : > "$T/am.log"
open_webui
chk 'grep -q "me.weishu.kernelsu.ui.webui.WebUIActivity" "$T/am.log" && grep -q "category.LAUNCHER -p com.sukisu.ultra" "$T/am.log"'
rm -f "$T/am.err"
chk 'grep -q "open_req_pending && open_webui" "$T/mod/scripts/daemon.sh"'

[ "$fail" = 0 ] && echo "test-keepalive OK"
exit "$fail"
