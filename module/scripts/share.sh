#!/system/bin/sh
# XRouter: [已由 bin/xr 内置] 原宿主机共享目录准备与校验库（现由 rust/xr/src/vm/share.rs 实现）
exec "${MODDIR:-${0%/*}/..}/bin/xr" "$@"
