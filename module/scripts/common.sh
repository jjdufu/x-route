#!/system/bin/sh
# XRouter: [已由 bin/xr 内置] 原通用配置/日志/CIDR/系统工具库（现由 rust/xr 实现；手动调试可直接调用 bin/xr get|set|status-line）
exec "${MODDIR:-${0%/*}/..}/bin/xr" "$@"
