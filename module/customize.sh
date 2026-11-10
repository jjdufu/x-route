#!/system/bin/sh
# ==============================================================================
# x-route: SuKiSU / KernelSU Module Installation Script (customize.sh)
# ==============================================================================
SKIPUNZIP=0

ui_print "===================================================="
ui_print "  x-route 随身软路由与内核保活 (OnePlus 13) v1.0.1"
ui_print "  Author : jjdufu"
ui_print "  Target : OnePlus 13 (SM8750 / Android 15-16)"
ui_print "===================================================="

if [ "${ARCH}" != "arm64" ]; then
    abort "! Unsupported architecture: ${ARCH} (arm64 required)"
fi

# Module does not modify /system; mark skip_mount so metamodules never touch us
touch "${MODPATH}/skip_mount"

# Ensure module.prop is intact, readable, and contains complete Title & Author
if [ ! -s "${MODPATH}/module.prop" ] || ! grep -q "^name=" "${MODPATH}/module.prop" 2>/dev/null || ! grep -q "^author=" "${MODPATH}/module.prop" 2>/dev/null; then
    cat <<'EOF' > "${MODPATH}/module.prop"
id=x_route
name=x-route 随身软路由与内核保活 (OnePlus 13)
version=v1.0.1
versionCode=101
author=jjdufu
description=一加 13 (骁龙8至尊版 / Android 15-16) Gunyah/QEMU 硬件虚拟化 OpenWrt 随身软路由 (5G WAN + 192.168.100.1 热点接管 + 本机回流 + NAS 挂载) 与四重内核级应用保活引擎。
EOF
fi

PERSIST_DIR="/data/adb/x-route"
CONF_FILE="${PERSIST_DIR}/config/config.conf"
FW_VER_FILE="${PERSIST_DIR}/images/.fw_version"
mkdir -p "${PERSIST_DIR}/config" "${PERSIST_DIR}/images" "${PERSIST_DIR}/run" "${PERSIST_DIR}/logs" "${PERSIST_DIR}/bin"

# Clear any stale bootloop fuse on reinstall/upgrade
rm -f "${PERSIST_DIR}/config/vm_booting.fuse" "${PERSIST_DIR}/config/bootloop_tripped.flag"

# Initialize or safely migrate existing user configuration
if [ ! -f "${CONF_FILE}" ] && [ -f "${MODPATH}/config/config.conf" ]; then
    ui_print "- Initializing default configuration at ${CONF_FILE}"
    cp -f "${MODPATH}/config/config.conf" "${CONF_FILE}"
else
    ui_print "- Checking existing configuration at ${CONF_FILE}"
    if ! grep -q '^CONFIG_SCHEMA_VER="101"' "${CONF_FILE}" 2>/dev/null; then
        ui_print "- Migrating v1.0.0 config to safe Gunyah v1.0.1 defaults (ROUTER_ENABLED=0, RAM=512MB, SWIOTLB=64MB)"
        sed -i 's/^ROUTER_ENABLED=.*/ROUTER_ENABLED="0"/' "${CONF_FILE}" 2>/dev/null || true
        sed -i 's/^VM_MEM_MB="256"/VM_MEM_MB="512"/' "${CONF_FILE}" 2>/dev/null || true
        sed -i 's/^NAS_MOUNT_ENABLED="1"/NAS_MOUNT_ENABLED="0"/' "${CONF_FILE}" 2>/dev/null || true
        grep -q '^VM_SWIOTLB_MB=' "${CONF_FILE}" 2>/dev/null || echo 'VM_SWIOTLB_MB="64"' >> "${CONF_FILE}"
        if grep -q '^CONFIG_SCHEMA_VER=' "${CONF_FILE}" 2>/dev/null; then
            sed -i 's/^CONFIG_SCHEMA_VER=.*/CONFIG_SCHEMA_VER="101"/' "${CONF_FILE}"
        else
            echo 'CONFIG_SCHEMA_VER="101"' >> "${CONF_FILE}"
        fi
    fi
fi

# Always update kernel Image when bundled in Full zip so Gunyah pVM fixes take effect
if [ -f "${MODPATH}/data/Image" ]; then
    ui_print "- Installing/Updating OpenWrt ARM64 Gunyah pVM Kernel (Image)..."
    cp -f "${MODPATH}/data/Image" "${PERSIST_DIR}/images/Image"
fi

# Extract bundled rootfs if missing or upgrading from pre-101 firmware
INSTALLED_FW="$(cat "${FW_VER_FILE}" 2>/dev/null || echo 0)"
if [ -f "${MODPATH}/data/openwrt-rootfs.img.gz" ]; then
    if [ ! -f "${PERSIST_DIR}/images/openwrt-rootfs.raw" ] || [ "${INSTALLED_FW}" != "101" ]; then
        ui_print "- Extracting OpenWrt ARM64 Rootfs to ${PERSIST_DIR}/images/openwrt-rootfs.raw..."
        gzip -dc "${MODPATH}/data/openwrt-rootfs.img.gz" > "${PERSIST_DIR}/images/openwrt-rootfs.raw"
        echo "101" > "${FW_VER_FILE}"
    else
        ui_print "- Keeping existing OpenWrt Rootfs at ${PERSIST_DIR}/images/openwrt-rootfs.raw"
    fi
fi

# Remove temporary bundled firmware payload from module dir to save ~100MB in /data/adb/modules
rm -rf "${MODPATH}/data"

# Detect Virtualization Capabilities on Device
if [ -e /dev/gunyah ]; then
    ui_print "- Detected Qualcomm Gunyah Hypervisor (/dev/gunyah) [Protected VM Ready]"
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
set_perm "${MODPATH}/module.prop" 0 0 0644
set_perm "${MODPATH}/service.sh" 0 0 0755
set_perm "${MODPATH}/action.sh" 0 0 0755
set_perm "${MODPATH}/uninstall.sh" 0 0 0755

# Enable KernelSU / SuKiSU Hot Install so Title, Author, WebUI & Action appear immediately without reboot
export MODULE_HOT_INSTALL_REQUEST=true

# Flush F2FS page cache immediately so module.prop & config survive any sudden power event
sync

ui_print "- Installation complete! Open WebUI or click 'Action' in SuKiSU Manager anytime."
