#!/system/bin/sh
# XRouter: 看门狗监控转发壳（周期巡检守护进程/网络/通知，供手动调试调用：sh scripts/watchdog.sh）
exec "${MODDIR:-${0%/*}/..}/bin/xr" internal watchdog "$@"
