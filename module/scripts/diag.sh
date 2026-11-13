#!/system/bin/sh
# XRouter: [已由 bin/xr 内置] 原诊断快照与日志导出库（现由 rust/xr/src/diag.rs 实现；手动调试可用 sh scripts/xr diag-export|log-export）
exec "${MODDIR:-${0%/*}/..}/bin/xr" internal diag-snap "$@"
