#!/system/bin/sh
# XRouter: 累计流量统计转发壳（供手动调试调用：sh scripts/traffic.sh json|reset|setup|hooks|snapshot|teardown）
exec "${MODDIR:-${0%/*}/..}/bin/xr" internal traffic "$@"
