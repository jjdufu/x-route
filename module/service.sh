#!/system/bin/sh
# XRouter: 开机服务（熔断检查 + 可选自启）
MODDIR="${0%/*}"
. "${MODDIR}/scripts/common.sh"
ensure_dirs
wait_boot_completed
is_module_disabled && exit 0
rm -f "${RUN}"/*.pid "${RUN}/crosvm.sock" "${RUN}/routes.active" "${RUN}/state"

if [ -f "${FUSE}" ]; then
  log_err service "上次启动 VM 后整机复位，熔断：关闭自启，需在 WebUI 确认"
  date '+%F %T' > "${TRIPPED}"; rm -f "${FUSE}"; set_kv AUTOSTART 0; sync
fi

load_config
# 无论是否自启（含熔断后），都拉起独立守护进程与看门狗（开机上下文，不依附管理器）
if [ -f "${MODDIR}/scripts/daemon.sh" ]; then
  # shellcheck disable=SC1091
  . "${MODDIR}/scripts/daemon.sh"
  doze_whitelist
  ensure_daemon || log_err service "守护进程未能启动"
  ensure_watchdog || log_err service "看门狗未能启动"
fi
[ "${AUTOSTART}" = 1 ] || { log_info service "AUTOSTART=0，不自启"; exit 0; }
[ -f "${TRIPPED}" ] && { log_info service "存在未确认的熔断，不自启"; exit 0; }
[ "$(cat "${LAST_GOOD}" 2>/dev/null)" = "$(config_hash)" ] || { log_info service "last_good 与当前配置不一致，跳过自启"; exit 0; }
sleep "${AUTOSTART_DELAY_SEC}"
until ce_ready; do sleep 5; done
sh "${MODDIR}/scripts/xr" start
