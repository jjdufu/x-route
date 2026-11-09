#!/system/bin/sh
# ==============================================================================
# x-route: SuKiSU Module Uninstallation Script (uninstall.sh)
# Restores system settings & cleans up runtime state upon module removal
# ==============================================================================

MODDIR="${0%/*}"
if [ -x "${MODDIR}/scripts/net_orchestrator.sh" ]; then
    "${MODDIR}/scripts/net_orchestrator.sh" stop 2>/dev/null || true
fi
if [ -x "${MODDIR}/scripts/vm_manager.sh" ]; then
    "${MODDIR}/scripts/vm_manager.sh" stop 2>/dev/null || true
fi
if [ -x "${MODDIR}/scripts/keepalive_daemon.sh" ]; then
    "${MODDIR}/scripts/keepalive_daemon.sh" stop 2>/dev/null || true
fi

# Restore Private DNS if saved
if [ -f /data/adb/x-route/run/orig_private_dns ]; then
    orig_dns="$(cat /data/adb/x-route/run/orig_private_dns 2>/dev/null)"
    [ -n "${orig_dns}" ] && settings put global private_dns_mode "${orig_dns}" 2>/dev/null || true
fi

rm -rf /data/adb/x-route/run
