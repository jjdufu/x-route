#!/system/bin/sh
# ==============================================================================
# x-route: Boot Auto-Start Service Script (service.sh)
# Executed by SuKiSU in late_start service mode with triple anti-bootloop guards
# ==============================================================================

MODDIR="${0%/*}"
export MODDIR
# shellcheck disable=SC1091
. "${MODDIR}/scripts/common.sh"

ensure_dirs
ensure_module_prop

if is_module_disabled; then
    log_info "service" "Module is disabled or marked for removal; exiting immediately."
    exit 0
fi

log_info "service" "x-route service.sh invoked; waiting for boot & FBE unlock..."
wait_until_boot_completed

if is_module_disabled; then
    log_info "service" "Module disabled during boot; exiting."
    exit 0
fi

# Give user a 15-second post-unlock safety window to toggle module in SuKiSU Manager
sleep 15
if is_module_disabled; then
    log_info "service" "Module disabled during post-unlock safety window; aborting start."
    exit 0
fi

load_config
log_info "service" "System boot completed. Initializing x-route v1.0.1 components..."

# 1. Start 4-Layer Kernel Keep-Alive Daemon
if [ "${KEEPALIVE_ENABLED:-1}" = "1" ]; then
    "${MODDIR}/scripts/keepalive_daemon.sh" start
fi

# 2. Hardware-Reset-Proof Bootloop Fuse Check before touching Hypervisor
if [ -f "${BOOT_FUSE_FILE}" ]; then
    log_err "service" "CRITICAL: Detected uncleaned vm_booting.fuse from previous boot! Auto-disabling ROUTER_ENABLED to prevent bootloop!"
    save_config_kv "ROUTER_ENABLED" "0"
    ROUTER_ENABLED="0"
    date '+%Y-%m-%d %H:%M:%S' > "${BOOT_TRIPPED_FILE}"
    rm -f "${BOOT_FUSE_FILE}"
    sync
fi

# 3. Start OpenWrt Soft Router & Network Orchestrator ONLY if explicitly enabled
if [ "${ROUTER_ENABLED:-0}" = "1" ]; then
    "${MODDIR}/scripts/net_orchestrator.sh" init-taps
    if "${MODDIR}/scripts/vm_manager.sh" start; then
        "${MODDIR}/scripts/net_orchestrator.sh" start
    else
        log_err "service" "OpenWrt VM failed to start; skipping network takeover."
    fi
else
    log_info "service" "ROUTER_ENABLED=0; skipping OpenWrt VM auto-start (start manually via WebUI or Action)."
fi

log_info "service" "x-route boot initialization finished."
