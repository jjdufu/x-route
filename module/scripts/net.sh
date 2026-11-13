#!/system/bin/sh
# XRouter: 宿主机网络规则与路由对账转发壳（供手动调试调用：sh scripts/net.sh up|down|watch|sync|status）
exec "${MODDIR:-${0%/*}/..}/bin/xr" internal net "$@"
