#!/system/bin/sh
# XRouter: [已由 bin/xr 内置] 原热点桥接与网段管理库（现由 rust/xr/src/net/hotspot.rs 实现；手动调试可用 sh scripts/net.sh sync|status）
exec "${MODDIR:-${0%/*}/..}/bin/xr" internal net "$@"
