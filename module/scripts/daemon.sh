#!/system/bin/sh
# XRouter: 常驻守护进程转发壳（内部命令轮询与状态维护，供手动调试调用：sh scripts/daemon.sh）
exec "${MODDIR:-${0%/*}/..}/bin/xr" internal daemon "$@"
