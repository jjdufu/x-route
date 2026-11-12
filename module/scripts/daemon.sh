#!/system/bin/sh
# XRouter: 独立守护进程。WebUI / ksu.exec 只向这里发命令，不直接拉起 crosvm。
# 必须以新会话运行，并脱离管理器 cgroup；仅 setsid/nohup 不够。
# 常驻通知由 LSPosed 式通知进程（app_process，android 身份）发出，见 keepalive.sh。
# shellcheck disable=SC2034
MODDIR="${MODDIR:-/data/adb/modules/xrouter}"
. "${MODDIR}/scripts/common.sh"
. "${MODDIR}/scripts/keepalive.sh"
. "${MODDIR}/scripts/traffic.sh"
. "${MODDIR}/scripts/diag.sh"

daemon_pid() { cat "${RUN}/daemon.pid" 2>/dev/null; }

daemon_running() {
  p="$(daemon_pid)"
  is_pid_running "$p" || return 1
  if [ -r "/proc/$p/cmdline" ]; then
    grep -q daemon.sh "/proc/$p/cmdline" 2>/dev/null || return 1
  fi
  return 0
}

_spawn_daemon() {
  if command -v setsid >/dev/null 2>&1; then
    setsid nohup sh "${MODDIR}/scripts/daemon.sh" </dev/null >/dev/null 2>&1 &
  elif [ -n "${BB}" ] && [ -x "${BB}" ]; then
    "${BB}" setsid nohup sh "${MODDIR}/scripts/daemon.sh" </dev/null >/dev/null 2>&1 &
  else
    nohup sh "${MODDIR}/scripts/daemon.sh" </dev/null >/dev/null 2>&1 &
  fi
}

ensure_watchdog() {
  [ "${XR_IN_WATCHDOG}" = 1 ] && return 0
  [ "${XR_DAEMON_FOREGROUND}" = 1 ] && return 0
  p="$(cat "${RUN}/watchdog.pid" 2>/dev/null)"
  if is_pid_running "$p" && [ -r "/proc/$p/cmdline" ]; then
    grep -q watchdog.sh "/proc/$p/cmdline" 2>/dev/null && { cgroup_escape "$p"; return 0; }
  fi
  if command -v setsid >/dev/null 2>&1; then
    setsid nohup sh "${MODDIR}/scripts/watchdog.sh" </dev/null >/dev/null 2>&1 &
  else
    nohup sh "${MODDIR}/scripts/watchdog.sh" </dev/null >/dev/null 2>&1 &
  fi
  t=0
  while [ "$t" -lt 8 ]; do
    p="$(cat "${RUN}/watchdog.pid" 2>/dev/null)"
    is_pid_running "$p" && { cgroup_escape "$p"; return 0; }
    sleep 1
    t=$((t + 1))
  done
  log_err daemon "看门狗未能启动"
  return 1
}

ensure_daemon() {
  [ "${XR_DAEMON_FOREGROUND}" = 1 ] && return 0
  ensure_dirs
  if daemon_running; then
    cgroup_escape_tree
    ensure_watchdog
    return 0
  fi
  rm -f "${RUN}/daemon.ready" "${RUN}/daemon.res"
  _spawn_daemon
  t=0
  while [ "$t" -lt 10 ]; do
    if [ -f "${RUN}/daemon.ready" ] && daemon_running; then
      cgroup_escape_tree
      ensure_watchdog
      return 0
    fi
    sleep 1
    t=$((t + 1))
  done
  log_err daemon "守护进程未能启动"
  return 1
}

# $1=start|stop|ping|quit  等待守护进程写入 daemon.res 后返回其退出码
daemon_send() {
  [ "${XR_DAEMON_FOREGROUND}" = 1 ] && return 0
  ensure_daemon || return 1
  rm -f "${RUN}/daemon.res"
  echo "$1" > "${RUN}/daemon.cmd"
  p="$(daemon_pid)"
  kill -USR1 "$p" 2>/dev/null || return 1
  t=0
  while [ "$t" -lt 30 ]; do
    [ -f "${RUN}/daemon.res" ] && break
    sleep 1
    t=$((t + 1))
  done
  rc="$(cat "${RUN}/daemon.res" 2>/dev/null)"
  [ -n "$rc" ] || return 1
  return "$rc"
}

daemon_handle() {
  cmd="$(cat "${RUN}/daemon.cmd" 2>/dev/null)"
  : > "${RUN}/daemon.cmd"
  rc=0
  case "$cmd" in
    start) vm_start; rc=$? ;;
    stop)  DAEMON_STOPPING=1; vm_stop; rc=$?; DAEMON_STOPPING=0 ;;
    ping)  rc=0 ;;
    quit)
      DAEMON_QUITTING=1
      vm_stop
      acct_snapshot
      notify_cancel
      [ "$(cat "${RUN}/daemon.pid" 2>/dev/null)" = "$$" ] && rm -f "${RUN}/daemon.pid" "${RUN}/daemon.ready"
      rm -f "${RUN}/daemon.cmd"
      echo 0 > "${RUN}/daemon.res"
      exit 0
      ;;
    "" ) return 0 ;;
    *) rc=2 ;;
  esac
  echo "$rc" > "${RUN}/daemon.res"
  if [ "${DAEMON_DEFER_TERM:-0}" = 1 ]; then
    DAEMON_QUITTING=1
    log_info daemon "停止流程完成，响应 TERM 退出"
    [ "$(cat "${RUN}/daemon.pid" 2>/dev/null)" = "$$" ] && rm -f "${RUN}/daemon.pid" "${RUN}/daemon.ready"
    exit 0
  fi
  : > "${RUN}/notify.force"
  notify_update
}

_daemon_on_signal() {
  [ "${DAEMON_QUITTING:-0}" = 1 ] && return 0
  if [ "${DAEMON_STOPPING:-0}" = 1 ] && [ "$1" = TERM ] && ! sys_shutting_down; then
    DAEMON_DEFER_TERM=1
    return 0
  fi
  DAEMON_QUITTING=1
  if [ "$1" = TERM ] || sys_shutting_down; then
    # TERM：关机时 init 群发，或卸载 / 人为结束。不算虚拟机崩溃：不写 last_error，虚拟机已不在时安静撤规则
    if sys_shutting_down; then shutdown_note daemon; else log_info daemon "收到 TERM，守护进程退出"; fi
    if ! xr_vm_alive; then rm -f "${RUN}/state" "${RUN}/crosvm.pid" "${RUN}/stopping"; routes_emergency_down; fi
  else
    emergency_teardown "收到信号 $1"
  fi
  [ "$(cat "${RUN}/daemon.pid" 2>/dev/null)" = "$$" ] && rm -f "${RUN}/daemon.pid" "${RUN}/daemon.ready"
  exit 0
}

_daemon_on_exit() {
  _rc=$?
  [ "${DAEMON_QUITTING:-0}" = 1 ] && return 0
  DAEMON_QUITTING=1
  emergency_teardown "进程退出 rc=${_rc}"
  [ "$(cat "${RUN}/daemon.pid" 2>/dev/null)" = "$$" ] && rm -f "${RUN}/daemon.pid" "${RUN}/daemon.ready"
}

daemon_loop() {
  ensure_dirs
  echo "$$" > "${RUN}/daemon.pid"
  cgroup_escape $$
  # shellcheck disable=SC3045
  trap '' HUP
  trap 'daemon_handle' USR1
  trap 'daemon_handle' USR2
  trap '_daemon_on_signal TERM' TERM
  trap '_daemon_on_signal INT' INT
  trap '_daemon_on_exit' EXIT
  : > "${RUN}/daemon.cmd"
  # 新守护进程重新核对通知（上一个进程留下的状态不可信）
  rm -f "${RUN}/notify.on" "${RUN}/notify.idle" "${RUN}/notify.last" "${RUN}/notify.ts" "${RUN}/notify.warned"
  echo ready > "${RUN}/daemon.ready"
  log_info daemon "已启动 pid=$$ cgroup=$(cgroup_path $$ 2>/dev/null)"
  while :; do
    # 系统关机：不再巡检（init 正在结束所有进程，虚拟机退出是预期的），记一行后安静退出
    if sys_shutting_down; then
      DAEMON_QUITTING=1; shutdown_note daemon
      [ "$(cat "${RUN}/daemon.pid" 2>/dev/null)" = "$$" ] && rm -f "${RUN}/daemon.pid" "${RUN}/daemon.ready"
      exit 0
    fi
    cgroup_escape_tree
    if ! vm_stopping && { [ -f "${RUN}/crosvm.pid" ] || [ -f "${RUN}/state" ]; }; then
      if ! vm_running 2>/dev/null; then
        emergency_teardown "$(vm_exit_reason)"
      elif [ "$(cat "${RUN}/state" 2>/dev/null)" = ready ] && ! is_pid_running "$(cat "${RUN}/fuse_watch.pid" 2>/dev/null)"; then
        load_config
        _cp="$(cat "${RUN}/crosvm.pid" 2>/dev/null)"
        _st="$(awk '{print $22}' "/proc/${_cp}/stat" 2>/dev/null)"
        _now="$(cut -d. -f1 /proc/uptime 2>/dev/null)"
        if [ -n "$_st" ] && [ -n "$_now" ] && [ $(( _now - _st / 100 )) -ge "${STABLE_SEC:-180}" ]; then
          rm -f "${FUSE}" "${BOOT_LOG}"; config_hash > "${LAST_GOOD}"; sync
          echo stable > "${RUN}/state"; log_info vm "稳定，熔断已解除，last_good 已更新"
        fi
      fi
    fi
    if stale_gateway; then
      emergency_teardown "$(vm_exit_reason)（xr0 无载波或虚拟机已死，规则仍在）"
    fi
    notify_update; rotate_all_logs
    # 大约每分钟把累计流量写一次快照（内容不变不写盘）
    _tick=$(( ${_tick:-0} + 1 ))
    if [ $((_tick % 12)) -eq 0 ]; then
      acct_snapshot
      load_config; debug_expire   # 调试模式到期自动关闭
    fi
    # 5 秒一轮，但每秒看一眼立即刷新请求（只做 [ -f ] 判断，不调外部命令）
    _dl=0
    while [ "$_dl" -lt 5 ]; do
      sleep 1
      _dl=$((_dl + 1))
      [ -f "${RUN}/notify.force" ] && notify_update
    done
  done
}

if [ "${0##*/}" = daemon.sh ]; then
  . "${MODDIR}/scripts/vm.sh"
  XR_IN_DAEMON=1
  export XR_IN_DAEMON
  daemon_loop
fi
