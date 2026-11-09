#!/system/bin/sh
# ==============================================================================
# x-route: SuKiSU / KernelSU Module Installation Script (customize.sh)
# ==============================================================================
SKIPUNZIP=0

ui_print "===================================================="
ui_print "  x-route: Pocket OpenWrt & 4-Layer Keep-Alive"
ui_print "  Target : OnePlus 13 (SM8750 / Android 15·16)"
ui_print "===================================================="

if [ "${ARCH}" != "arm64" ]; then
    abort "! Unsupported architecture: ${ARCH} (arm64 required)"
fi

PERSIST_DIR="/data/adb/x-route"
mkdir -p "${PERSIST_DIR}/config" "${PERSIST_DIR}/images" "${PERSIST_DIR}/run" "${PERSIST_DIR}/logs" "${PERSIST_DIR}/bin"

# Preserve existing user config if upgrading
if [ ! -f "${PERSIST_DIR}/config/config.conf" ] && [ -f "${MODPATH}/config/config.conf" ]; then
    ui_print "- Initializing default configuration at ${PERSIST_DIR}/config/config.conf"
    cp -f "${MODPATH}/config/config.conf" "${PERSIST_DIR}/config/config.conf"
else
    ui_print "- Preserving existing configuration at ${PERSIST_DIR}/config/config.conf"
fi

# Extract bundled firmware if included in module zip and not yet present
if [ -f "${MODPATH}/data/Image" ] && [ ! -f "${PERSIST_DIR}/images/Image" ]; then
    ui_print "- Installing OpenWrt ARM64 Kernel (Image)..."
    cp -f "${MODPATH}/data/Image" "${PERSIST_DIR}/images/Image"
fi
if [ -f "${MODPATH}/data/openwrt-rootfs.img.gz" ] && [ ! -f "${PERSIST_DIR}/images/openwrt-rootfs.raw" ]; then
    ui_print "- Extracting OpenWrt ARM64 Rootfs to ${PERSIST_DIR}/images/openwrt-rootfs.raw..."
    gzip -dc "${MODPATH}/data/openwrt-rootfs.img.gz" > "${PERSIST_DIR}/images/openwrt-rootfs.raw"
fi

# Detect Virtualization Capabilities on Device
if [ -e /dev/gunyah ]; then
    ui_print "- Detected Qualcomm Gunyah Hypervisor (/dev/gunyah) [Hardware Virtualization Ready]"
elif [ -e /dev/kvm ]; then
    ui_print "- Detected Linux KVM (/dev/kvm) [Hardware Virtualization Ready]"
else
    ui_print "- Notice: Neither /dev/gunyah nor /dev/kvm found; QEMU MTTCG fallback will be used"
fi

if [ -x /apex/com.android.virt/bin/crosvm ]; then
    ui_print "- Detected system AVF crosvm (/apex/com.android.virt/bin/crosvm)"
fi

set_perm_recursive "${MODPATH}" 0 0 0755 0644
set_perm_recursive "${MODPATH}/scripts" 0 0 0755 0755
set_perm "${MODPATH}/service.sh" 0 0 0755
set_perm "${MODPATH}/action.sh" 0 0 0755
set_perm "${MODPATH}/uninstall.sh" 0 0 0755

ui_print "- Installation complete! Reboot or click 'Action' / 'WebUI' in SuKiSU Manager."
