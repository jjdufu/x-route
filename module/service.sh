#!/system/bin/sh
# ==============================================================================
# x-route: Boot Auto-Start Service Script (service.sh)
# Executed by SuKiSU in late_start service mode
# ==============================================================================

MODDIR="${0%/*}"
export MODDIR
# shellcheck disable=SC1091
. "${MODDIR}/scripts/common.sh"

ensure_dirs
log_info "service" "x-route service.sh invoked; waiting for boot & FBE unlock..."

wait_until_boot_completed
load_config

log_info "service" "System boot completed. Initializing x-route components..."

# 1. Start 4-Layer Kernel Keep-Alive Daemon
if [ "${KEEPALIVE_ENABLED:-1}" = "1" ]; then
    "${MODDIR}/scripts/keepalive_daemon.sh" start
fi

# 2. Start OpenWrt Soft Router & Network Orchestrator if enabled
if [ "${ROUTER_ENABLED:-1}" = "1" ]; then
    "${MODDIR}/scripts/net_orchestrator.sh" init-taps
    "${MODDIR}/scripts/vm_manager.sh" start
    "${MODDIR}/scripts/net_orchestrator.sh" start
else
    log_info "service" "ROUTER_ENABLED=0; skipping OpenWrt VM auto-start."
fi

log_info "service" "x-route boot initialization finished."
