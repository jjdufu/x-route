#!/system/bin/sh
# XRouter: 保活辅助（cgroup 脱离、异常退出撤路由、常驻通知）。
# 由 daemon.sh / service.sh / watchdog.sh 引入。
# 常驻通知与 LSPosed 的 lspd 同法：root 经 app_process 跑 bin/xrnotify.jar，以 android 身份
# 直接调用 NotificationManagerService，不安装任何 App（见 notifier/）。
# shellcheck disable=SC2034

XR_NOTIFIER_JAR="${XR_NOTIFIER_JAR:-${MODDIR}/bin/xrnotify.jar}"
XR_APP_PROCESS="${XR_APP_PROCESS:-/system/bin/app_process}"
# 测试时可把 cgroup 根指到临时目录
XR_CGROUP_ROOT="${XR_CGROUP_ROOT:-/sys/fs/cgroup}"
XR_PROC_BASE="${XR_PROC_BASE:-/proc}"

# 把进程从管理器/应用 cgroup 挪到系统或根 cgroup，避免上滑或一键清理时被连带杀掉。
# 仅 setsid/nohup 不够：进程仍可能留在 SukiSU 的 apps/uid_*/pid_* 下。
cgroup_escape() {
  _pid="$1"
  [ -n "$_pid" ] || return 1
  is_pid_running "$_pid" || return 1
  _root="${XR_CGROUP_ROOT}"
  # 依次尝试：cgroup v2 根、system 片、uid_0；以及传统 memcg / cpuset
  for _f in \
      "${_root}/cgroup.procs" \
      "${_root}/system/cgroup.procs" \
      "${_root}/uid_0/cgroup.procs" \
      /dev/memcg/system/cgroup.procs \
      /dev/cpuset/foreground/cgroup.procs \
      /dev/cpuset/foreground/tasks \
      /dev/stune/foreground/cgroup.procs \
      /dev/stune/foreground/tasks
  do
    [ -w "$_f" ] || continue
    # 只要有一处写入成功即记下，其余能写也写（cpu/mem 控制器可能分挂）
    echo "$_pid" > "$_f" 2>/dev/null || continue
  done
  echo -1000 > "${XR_PROC_BASE}/${_pid}/oom_score_adj" 2>/dev/null
  return 0
}

# 虚拟机是否在跑。看门狗不引入 vm.sh 时也必须可靠：未定义 vm_running 就直接看 crosvm.pid。
# 不能把“函数不存在”当成“虚拟机已死”，否则会误撤 xr0，导致 crosvm 写 tap 报 EBADFD、整机断网。
xr_vm_alive() {
  if type vm_running >/dev/null 2>&1; then
    vm_running 2>/dev/null
    return $?
  fi
  _p="$(cat "${RUN}/crosvm.pid" 2>/dev/null)"
  [ -n "$_p" ] || return 1
  [ -d "${XR_PROC_BASE}/${_p}" ] || return 1
  grep -q crosvm "${XR_PROC_BASE}/${_p}/cmdline" 2>/dev/null
}

# 当前 cgroup 路径（去掉控制器前缀），供测试与日志
cgroup_path() {
  _pid="$1"
  _f="${XR_PROC_BASE}/${_pid}/cgroup"
  [ -r "$_f" ] || return 1
  sed -n 's/^0:://p; s/^[^:]*:[^:]*://p' "$_f" 2>/dev/null | head -1
}

# 进程是否仍落在应用/管理器 cgroup 里
cgroup_in_apps() {
  _p="$(cgroup_path "$1")" || return 1
  case "$_p" in
    /apps/*|apps/*|*/apps/uid_*|*/uid_*/pid_*|*suki*) return 0 ;;
  esac
  return 1
}

# 把守护进程及其子进程（crosvm / net_watch / fuse_watch）一并脱离
cgroup_escape_tree() {
  cgroup_escape "${1:-$$}"
  for _f in "${RUN}/crosvm.pid" "${RUN}/net_watch.pid" "${RUN}/fuse_watch.pid" "${RUN}/watchdog.pid" "${RUN}/daemon.pid" "${RUN}/notifier.pid"; do
    _p="$(cat "$_f" 2>/dev/null)"
    [ -n "$_p" ] && cgroup_escape "$_p"
  done
}

# 虚拟机状态文案
notify_state() {
  case "$(cat "${RUN}/state" 2>/dev/null)" in
    stable) echo "运行中" ;;
    ready) echo "已就绪" ;;
    *) echo "启动中" ;;
  esac
}

# 通知负载：状态 显示项 公网上传 公网下载 内网上传 内网下载（以空格分隔，供比较是否变化）
notify_payload() {
  _m="${XR_NOTIFY_TRAFFIC:-total}"
  case "$_m" in total|wan|lan) ;; *) _m=total ;; esac
  _t="0 0 0 0"
  type acct_total >/dev/null 2>&1 && _t="$(acct_total)"
  echo "$(notify_state) ${_m} ${_t}"
}

# 亮屏判断：dumpsys power 的 mWakefulness；拿不到时按亮屏处理（宁可多刷一次也不停更）
screen_on() {
  [ -n "${XR_SCREEN:-}" ] && { [ "${XR_SCREEN}" = on ]; return; }
  _w="$(dumpsys power 2>/dev/null | sed -n 's/.*mWakefulness=\([A-Za-z]*\).*/\1/p' | head -1)"
  case "$_w" in Asleep|Dozing) return 1 ;; esac
  return 0
}

# 通知进程是否在跑（--nice-name=xrnotify，命令行里只剩这个名字）
notifier_running() {
  _p="$(cat "${RUN}/notifier.pid" 2>/dev/null)"
  [ -n "$_p" ] || return 1
  is_pid_running "$_p" || return 1
  grep -q xrnotify "${XR_PROC_BASE}/${_p}/cmdline" 2>/dev/null
}

# $1=serve|cancel  经 app_process 跑通知进程（与 lspd 相同的启动方式）
_notifier_exec() {
  "${XR_APP_PROCESS}" -Djava.class.path="${XR_NOTIFIER_JAR}" /system/bin \
    --nice-name=xrnotify com.xrouter.notify.Main "$1" "${RUN}"
}

# 拉起常驻通知进程（新会话，脱离管理器 cgroup）；诊断输出留在 run/notifier.log
notifier_start() {
  notifier_running && return 0
  [ -f "${XR_NOTIFIER_JAR}" ] || return 1
  rm -f "${RUN}/notify.req" "${RUN}/notify.ack"
  if command -v setsid >/dev/null 2>&1; then
    setsid "${XR_APP_PROCESS}" -Djava.class.path="${XR_NOTIFIER_JAR}" /system/bin \
      --nice-name=xrnotify com.xrouter.notify.Main serve "${RUN}" </dev/null >/dev/null 2>"${RUN}/notifier.log" &
  else
    _notifier_exec serve </dev/null >/dev/null 2>"${RUN}/notifier.log" &
  fi
  echo "$!" > "${RUN}/notifier.pid"
  cgroup_escape "$!"
  return 0
}

# $1=一行内容  原子写给通知进程
_notifier_req() {
  echo "$1" > "${RUN}/notify.req.tmp" && mv -f "${RUN}/notify.req.tmp" "${RUN}/notify.req"
}

# $1=期望的回执  最多等 XR_NOTIFY_WAIT（默认 25）× 0.2 秒；首次拉起时 JVM 启动要一两秒
_notifier_wait() {
  _i=0
  while [ "$_i" -lt "${XR_NOTIFY_WAIT:-25}" ]; do
    _a="$(cat "${RUN}/notify.ack" 2>/dev/null)"
    [ "$_a" = "$1" ] && return 0
    case "$_a" in "err "*) return 1 ;; esac
    sleep 0.2 2>/dev/null || sleep 1
    _i=$((_i + 1))
  done
  return 1
}

# $1=负载  发出或更新常驻通知；通知进程回执 "ok <负载>" 才算成功
notifier_post() {
  notifier_start || return 1
  _notifier_req "$1" || return 1
  _notifier_wait "ok $1"
}

# 撤通知：通知进程在跑就让它撤掉并退出；不在跑（或没回执）则一次性拉起撤销，
# 也能清掉上一个守护进程崩溃后留下的通知（android 身份发的通知不随进程退出消失）。
notify_cancel() {
  _done=0
  if notifier_running; then
    _notifier_req cancel && _notifier_wait "ok cancel" && _done=1
    _p="$(cat "${RUN}/notifier.pid" 2>/dev/null)"
    [ -n "$_p" ] && kill "$_p" 2>/dev/null
  fi
  if [ "$_done" = 0 ] && [ -f "${XR_NOTIFIER_JAR}" ]; then
    _notifier_exec cancel </dev/null >/dev/null 2>>"${RUN}/notifier.log"
  fi
  rm -f "${RUN}/notifier.pid" "${RUN}/notify.req" "${RUN}/notify.ack" \
    "${RUN}/notify.on" "${RUN}/notify.last" "${RUN}/notify.ts"
  return 0
}

# 守护进程每轮调用；大约每 XR_NOTIFY_INTERVAL（默认 10）秒刷新一次累计流量。
# 只在内容变化时推送；息屏时暂停（通知保留最后的数值）；notify.force 存在时立即刷新（改设置、清零）。
# 通知进程死了（如 system_server 重启）下一轮重新拉起并重发。
notify_update() {
  if ! xr_vm_alive; then
    # 只在确实发过通知时撤一次，不每轮都拉 JVM
    if [ -f "${RUN}/notify.on" ] || [ ! -f "${RUN}/notify.idle" ]; then
      notify_cancel
      : > "${RUN}/notify.idle"
    fi
    return 0
  fi
  rm -f "${RUN}/notify.idle"
  _force=0
  [ -f "${RUN}/notify.force" ] && { rm -f "${RUN}/notify.force"; _force=1; }
  if [ -f "${RUN}/notify.on" ] && ! notifier_running; then
    rm -f "${RUN}/notify.on"
  fi
  _now="$(date +%s)"
  _ts="$(cat "${RUN}/notify.ts" 2>/dev/null)"; case "$_ts" in ''|*[!0-9]*) _ts=0 ;; esac
  if [ -f "${RUN}/notify.on" ] && [ "$_force" = 0 ]; then
    [ $((_now - _ts)) -lt "${XR_NOTIFY_INTERVAL:-10}" ] && return 0
  fi
  load_config 2>/dev/null
  _pl="$(notify_payload)"
  if [ -f "${RUN}/notify.on" ] && [ "$_force" = 0 ]; then
    if [ "$_pl" = "$(cat "${RUN}/notify.last" 2>/dev/null)" ] || ! screen_on; then
      echo "$_now" > "${RUN}/notify.ts"
      return 0
    fi
  fi
  if notifier_post "$_pl"; then
    echo "$_pl" > "${RUN}/notify.last"; echo "$_now" > "${RUN}/notify.ts"; : > "${RUN}/notify.on"
    rm -f "${RUN}/notify.warned"
    return 0
  fi
  echo "$_now" > "${RUN}/notify.ts"
  if [ ! -f "${RUN}/notify.warned" ]; then
    _why="$(cat "${RUN}/notify.ack" 2>/dev/null)"
    [ -n "$_why" ] || _why="$(tail -n 3 "${RUN}/notifier.log" 2>/dev/null | tr '\n' ' ')"
    [ -n "$_why" ] || _why="无回执"
    log_err daemon "常驻通知发送失败：$(echo "$_why" | cut -c1-200)"
    : > "${RUN}/notify.warned"
  fi
  return 1
}

# 虚拟机意外退出时尽量写出原因
vm_exit_reason() {
  _p="$(cat "${RUN}/crosvm.pid" 2>/dev/null)"
  if [ -n "$_p" ] && [ -r "${XR_PROC_BASE}/${_p}/cmdline" ]; then
    echo "crosvm 仍在运行"
    return 0
  fi
  if [ -f "${XR_DATA}/log/vmm.log" ]; then
    _tail="$(tail -n 8 "${XR_DATA}/log/vmm.log" 2>/dev/null | tr '\n' ' ' | tr -s ' ')"
    [ -n "$_tail" ] && { echo "crosvm 已退出：${_tail}"; return 0; }
  fi
  echo "crosvm 已退出，日志未记录退出原因"
}

# 立刻撤销全局路由与规则，避免 19100 仍指向已断开的 xr0
routes_emergency_down() {
  if [ -f "${MODDIR}/scripts/net.sh" ]; then
    sh "${MODDIR}/scripts/net.sh" down >/dev/null 2>&1
  fi
}

# $1=原因  守护进程或虚拟机异常退出时调用
emergency_teardown() {
  _why="${1:-未知原因}"
  # 系统关机：虚拟机被 init 一起结束，不是崩溃。只撤规则、记一行 INFO，不写 last_error
  if sys_shutting_down; then
    shutdown_note daemon
    xr_vm_alive && return 0
    rm -f "${RUN}/state" "${RUN}/crosvm.pid"
    routes_emergency_down
    rm -f "${RUN}/net.gw"   # 关机中 net.sh down 可能失败；不再反复判定为残留规则
    return 0
  fi
  # 同一原因只记一次（规则撤不掉时守护进程 / 看门狗每轮都会走到这里，不能刷屏）；下次启动虚拟机时重置
  if [ "$(cat "${RUN}/teardown.last" 2>/dev/null)" != "${_why}" ]; then
    printf '%s\n' "${_why}" > "${RUN}/teardown.last"
    log_err daemon "异常退出：${_why}"
    type diag_snap >/dev/null 2>&1 && diag_snap "异常：${_why}" 3
  fi
  if xr_vm_alive; then
    # 虚拟机还在，只记原因，把路由留给仍在跑的 xr0；由看门狗拉起守护进程
    return 0
  fi
  echo "虚拟机意外退出" > "${RUN}/last_error"
  rm -f "${RUN}/state" "${RUN}/crosvm.pid"
  routes_emergency_down
  notify_cancel
}

# xr0 已无载波或网卡不在，但接管规则还在
stale_gateway() {
  [ -f "${RUN}/net.gw" ] || return 1
  if xr_vm_alive; then
    _car="${XR_SYSNET:-/sys/class/net}/xr0/carrier"
    [ -f "$_car" ] && [ "$(cat "$_car" 2>/dev/null)" = 0 ] && return 0
    return 1
  fi
  return 0
}
