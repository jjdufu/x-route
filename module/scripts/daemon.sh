#!/system/bin/sh
# XRouter: 独立守护进程。WebUI / ksu.exec 只向这里发命令，不直接拉起 crosvm。
# 以新会话运行，避免页面关闭时被一并结束。没有独立 APK，常驻通知用 cmd notification。
# shellcheck disable=SC2034
MODDIR="${MODDIR:-/data/adb/modules/xrouter}"
. "${MODDIR}/scripts/common.sh"

daemon_pid() { cat "${RUN}/daemon.pid" 2>/dev/null; }

daemon_running() {
  p="$(daemon_pid)"
  is_pid_running "$p" || return 1
  if [ -r "/proc/$p/cmdline" ]; then
    grep -q daemon.sh "/proc/$p/cmdline" 2>/dev/null || return 1
  fi
  return 0
}

notify_cancel() {
  cmd notification cancel xrouter >/dev/null 2>&1
  cmd notification cancel tag xrouter >/dev/null 2>&1
  return 0
}

notify_update() {
  if vm_running 2>/dev/null; then
    if ! cmd notification post --ongoing -t XRouter xrouter "运行中" >/dev/null 2>&1; then
      cmd notification post -t XRouter xrouter "运行中" >/dev/null 2>&1 || \
        log_info daemon "无法发送通知"
    fi
  else
    notify_cancel
  fi
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

ensure_daemon() {
  [ "${XR_DAEMON_FOREGROUND}" = 1 ] && return 0
  ensure_dirs
  daemon_running && return 0
  rm -f "${RUN}/daemon.ready" "${RUN}/daemon.res"
  _spawn_daemon
  t=0
  while [ "$t" -lt 10 ]; do
    if [ -f "${RUN}/daemon.ready" ] && daemon_running; then return 0; fi
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

daemon_loop() {
  ensure_dirs
  echo "$$" > "${RUN}/daemon.pid"
  # shellcheck disable=SC3045
  trap '' HUP
  trap 'daemon_handle' USR1
  trap 'daemon_handle' USR2
  : > "${RUN}/daemon.cmd"
  echo ready > "${RUN}/daemon.ready"
  log_info daemon "已启动 pid=$$"
  while :; do
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
