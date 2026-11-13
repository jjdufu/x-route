#!/system/bin/sh
# XRouter: [已由 bin/xr 内置] 原 cgroup 脱离与通知推流保活库（现由 rust/xr/src/daemon/keepalive.rs 实现）
exec "${MODDIR:-${0%/*}/..}/bin/xr" "$@"
