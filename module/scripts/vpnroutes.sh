#!/system/bin/sh
# XRouter: [已由 bin/xr 内置] 原 VPN 路由表同步与 CIDR 校验库（现由 rust/xr/src/net/vpn_routes.rs 实现；手动调试可用 sh scripts/net.sh sync）
exec "${MODDIR:-${0%/*}/..}/bin/xr" internal net "$@"
