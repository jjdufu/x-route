#!/system/bin/sh
# XRouter: 开机服务入口（熔断检查、拉起守护进程与看门狗、可选自启；转发至 bin/xr）
exec "${0%/*}/bin/xr" internal boot-service
