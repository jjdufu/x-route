#!/system/bin/sh
# x-route v2: 管理器“操作”按钮：运行中则停止，否则启动
MODDIR="${0%/*}"
if sh "${MODDIR}/scripts/xr" running; then sh "${MODDIR}/scripts/xr" stop; else sh "${MODDIR}/scripts/xr" start; fi
sh "${MODDIR}/scripts/xr" status-line
