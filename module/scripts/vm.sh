#!/system/bin/sh
# XRouter: 虚拟机生命周期管理转发壳（供手动调试调用：sh scripts/vm.sh start|stop|status|expand|reset-disk）
exec "${MODDIR:-${0%/*}/..}/bin/xr" internal vm "$@"
