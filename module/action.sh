#!/system/bin/sh
# XRouter: 管理器「操作」按钮入口（切换虚拟机启停并输出状态行；转发至 bin/xr）
exec "${0%/*}/bin/xr" internal action-toggle
