#!/system/bin/sh
# XRouter: [已由 bin/xr 内置] 原 IPv6 代理/地址计算与路由库（现由 rust/xr/src/net/ipv6.rs 实现；手动调试可用 sh scripts/xr ipv6 on|off）
exec "${MODDIR:-${0%/*}/..}/bin/xr" internal net "$@"
