#!/system/bin/sh
# XRouter: 独立守护进程。WebUI / ksu.exec 只向这里发命令，不直接拉起 crosvm。
# 必须以新会话运行，并脱离管理器 cgroup；仅 setsid/nohup 不够。
# 常驻通知优先用伴生前台服务，cmd notification 只作回退且必须校验是否真的显示。
# shellcheck disable=SC2034
MODDIR="${MODDIR:-/data/adb/modules/xrouter}"
. "${MODDIR}/scripts/common.sh"
. "${MODDIR}/scripts/keepalive.sh"

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
    stop)  vm_stop; rc=$? ;;
    ping)  rc=0 ;;
    quit)
      DAEMON_QUITTING=1
      vm_stop
      notify_cancel
      rm -f "${RUN}/daemon.pid" "${RUN}/daemon.ready" "${RUN}/daemon.cmd"
      echo 0 > "${RUN}/daemon.res"
      exit 0
      ;;
    "" ) return 0 ;;
    *) rc=2 ;;
  esac
  echo "$rc" > "${RUN}/daemon.res"
  notify_update
}

_daemon_on_signal() {
  [ "${DAEMON_QUITTING}" = 1 ] && return 0
  DAEMON_QUITTING=1
  emergency_teardown "收到信号 $1"
  rm -f "${RUN}/daemon.pid" "${RUN}/daemon.ready"
  exit 0
}

_daemon_on_exit() {
  _rc=$?
  [ "${DAEMON_QUITTING}" = 1 ] && return 0
  DAEMON_QUITTING=1
  emergency_teardown "进程退出 rc=${_rc}"
  rm -f "${RUN}/daemon.pid" "${RUN}/daemon.ready"
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
  echo ready > "${RUN}/daemon.ready"
  log_info daemon "已启动 pid=$$ cgroup=$(cgroup_path $$ 2>/dev/null)"
  while :; do
    cgroup_escape_tree
    if [ -f "${RUN}/crosvm.pid" ] || [ -f "${RUN}/state" ]; then
      if ! vm_running 2>/dev/null; then
        emergency_teardown "$(vm_exit_reason)"
      fi
    fi
    if stale_gateway; then
      emergency_teardown "$(vm_exit_reason)（xr0 无载波或虚拟机已死，规则仍在）"
    fi
    notify_update; rotate_all_logs
    sleep 5
  done
}

if [ "${0##*/}" = daemon.sh ]; then
  . "${MODDIR}/scripts/vm.sh"
  XR_IN_DAEMON=1
  export XR_IN_DAEMON
  daemon_loop
fi
