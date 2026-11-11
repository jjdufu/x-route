#!/system/bin/sh
# XRouter: 开机上下文看门狗。不依附 SukiSU 管理器进程。
# 发现守护进程或虚拟机异常退出时立刻撤路由并记下原因，再按需拉起守护进程。
MODDIR="${MODDIR:-/data/adb/modules/xrouter}"
. "${MODDIR}/scripts/common.sh"
. "${MODDIR}/scripts/keepalive.sh"
. "${MODDIR}/scripts/daemon.sh"

watchdog_loop() {
  ensure_dirs
  echo "$$" > "${RUN}/watchdog.pid"
  cgroup_escape $$
  trap '' HUP
  trap 'rm -f "${RUN}/watchdog.pid"; exit 0' TERM INT
  log_info watchdog "已启动 pid=$$"
  while :; do
    is_module_disabled && { sleep 15; continue; }
    # 虚拟机已死或 xr0 无载波，但规则还在：立刻撤掉，避免银行/微信一直等到超时
    if stale_gateway; then
      emergency_teardown "$(vm_exit_reason)（看门狗）"
    fi
    if ! daemon_running; then
      log_info watchdog "守护进程不在，重新拉起"
      ensure_daemon
    else
      cgroup_escape_tree
    fi
    rotate_all_logs
    sleep "${XR_WATCHDOG_INTERVAL:-10}"
  done
}

if [ "${0##*/}" = watchdog.sh ]; then
  watchdog_loop
fi
