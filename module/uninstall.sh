#!/system/bin/sh
# XRouter: 卸载：停止 VM、撤销网络、删除全部数据（包括系统盘）
MODDIR="${0%/*}"
sh "${MODDIR}/scripts/xr" stop >/dev/null 2>&1
sh "${MODDIR}/scripts/net.sh" down >/dev/null 2>&1
{ echo xrouter > /sys/power/wake_unlock; } 2>/dev/null
rm -rf "${XR_DATA:-/data/adb/xrouter}"
