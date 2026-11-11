#!/system/bin/sh
# ==============================================================================
# XRouter: 安装脚本（只做全新安装，不做任何版本迁移）
# Full 包自带 data/Image 与 data/openwrt-rootfs.img.gz；
# Lite 包不带镜像，要求镜像已放在 /data/adb/xrouter/images/ 下。
# ==============================================================================
# shellcheck disable=SC2034
SKIPUNZIP=0
XR_DATA="${XR_DATA:-/data/adb/xrouter}"   # 测试时可覆盖
IMG="${XR_DATA}/images"
ROOTFS="${IMG}/openwrt-rootfs.img"

ui_print "===================================================="
ui_print "  XRouter (ImmortalWrt 25.12.2)"
ui_print "  Author : jjdufu"
ui_print "===================================================="
[ "${ARCH}" = "arm64" ] || abort "! 仅支持 arm64（当前 ${ARCH}）"
[ -x /apex/com.android.virt/bin/crosvm ] || ui_print "! 警告：未找到 /apex/com.android.virt/bin/crosvm，VM 将无法启动"

avail_mb="$(df -m /data | awk 'NR==2{print $4}')"
[ "${avail_mb:-0}" -ge 1536 ] || abort "! /data 剩余空间不足 1.5GB"

touch "${MODPATH}/skip_mount"
mkdir -p "${XR_DATA}/config" "${IMG}" "${XR_DATA}/run" "${XR_DATA}/log"
[ -f "${XR_DATA}/config/config.conf" ] || cp "${MODPATH}/config/config.conf" "${XR_DATA}/config/config.conf"

if [ -f "${MODPATH}/data/Image" ]; then
  cp -f "${MODPATH}/data/Image" "${IMG}/Image"
  cp -f "${MODPATH}/data/openwrt-rootfs.img.gz" "${IMG}/openwrt-rootfs.img.gz"   # 留给 xr reset-disk 使用
else
  ui_print "- Lite 包：使用 ${IMG} 中已有的镜像"
  [ -f "${IMG}/Image" ] || abort "! 缺少 ${IMG}/Image（请改用 full 包，或先放入镜像）"
  [ -f "${ROOTFS}" ] || [ -f "${IMG}/openwrt-rootfs.img.gz" ] || abort "! 缺少 ${IMG}/openwrt-rootfs.img.gz"
fi

if [ ! -f "${ROOTFS}" ]; then
  ui_print "- 解压系统盘"
  gzip -dc "${IMG}/openwrt-rootfs.img.gz" > "${ROOTFS}.new" || abort "! 解压失败"
  . "${XR_DATA}/config/config.conf"
  if ! truncate -s "${VM_DISK_SIZE_MB:-1024}M" "${ROOTFS}.new" || ! mv -f "${ROOTFS}.new" "${ROOTFS}"; then
    abort "! 系统盘创建失败"
  fi
else
  ui_print "- 保留已有系统盘 ${ROOTFS}"
fi
rm -rf "${MODPATH}/data"
sync

set_perm_recursive "${MODPATH}" 0 0 0755 0644
for f in xr vm.sh net.sh share.sh vpnroutes.sh common.sh daemon.sh v6.sh keepalive.sh watchdog.sh traffic.sh; do
  [ -f "${MODPATH}/scripts/${f}" ] && set_perm "${MODPATH}/scripts/${f}" 0 0 0755
done
chmod 0700 "${XR_DATA}"
ui_print "- 安装完成。VM 默认不启动，请在 WebUI 中手动启动。"
ui_print "- VM 地址 172.29.233.254，登录 root，上游默认空密码（可在 LuCI 设置）"
