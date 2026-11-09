#!/system/bin/sh
# ==============================================================================
# x-route: Common Functions, Constants, and Configuration Helpers
# ==============================================================================

export PATH="/data/adb/ap/bin:/data/adb/ksu/bin:/data/adb/magisk:/data/adb/x-route/bin:/apex/com.android.virt/bin:/system/bin:/system/xbin:/vendor/bin:/data/data/com.termux/files/usr/bin:$PATH"

MODDIR="${MODDIR:-/data/adb/modules/x_route}"
PERSIST_DIR="/data/adb/x-route"
CONF_DIR="${PERSIST_DIR}/config"
CONF_FILE="${CONF_DIR}/config.conf"
IMG_DIR="${PERSIST_DIR}/images"
RUN_DIR="${PERSIST_DIR}/run"
LOG_DIR="${PERSIST_DIR}/logs"
BIN_DIR="${PERSIST_DIR}/bin"

WAN_TAP="tap-wan"
LAN_TAP="tap-lan"
LAN_BR="br-lan"
WAN_MAC="52:54:00:12:34:56"
LAN_MAC="52:54:00:12:34:57"

ROUTE_TABLE_REDIRECT="100"
FWMARK_EXEMPT="0x100/0x100"
WAKELOCK_NAME="x_route_wakelock"

ensure_dirs() {
    mkdir -p "${CONF_DIR}" "${IMG_DIR}" "${RUN_DIR}" "${LOG_DIR}" "${BIN_DIR}"
    if [ ! -f "${CONF_FILE}" ] && [ -f "${MODDIR}/config/config.conf" ]; then
        cp -f "${MODDIR}/config/config.conf" "${CONF_FILE}"
    fi
}

load_config() {
    ensure_dirs
    if [ -f "${CONF_FILE}" ]; then
        # shellcheck disable=SC1090
        . "${CONF_FILE}"
    elif [ -f "${MODDIR}/config/config.conf" ]; then
        # shellcheck disable=SC1090
        . "${MODDIR}/config/config.conf"
    fi
}

save_config_kv() {
    local key="$1"
    local val="$2"
    ensure_dirs
    if grep -q "^${key}=" "${CONF_FILE}" 2>/dev/null; then
        sed -i "s|^${key}=.*|${key}=\"${val}\"|" "${CONF_FILE}"
    else
        echo "${key}=\"${val}\"" >> "${CONF_FILE}"
    fi
}

log_msg() {
    local level="$1"
    local comp="$2"
    shift 2
    local ts
    ts="$(date '+%Y-%m-%d %H:%M:%S')"
    ensure_dirs
    echo "[${ts}] [${level}] [${comp}] $*" >> "${LOG_DIR}/x-route.log"
    # Rotate log if > 1MB
    if [ -f "${LOG_DIR}/x-route.log" ]; then
        local sz
        sz="$(wc -c < "${LOG_DIR}/x-route.log" 2>/dev/null || echo 0)"
        if [ "${sz}" -gt 1048576 ]; then
            tail -n 500 "${LOG_DIR}/x-route.log" > "${LOG_DIR}/x-route.log.tmp" && mv -f "${LOG_DIR}/x-route.log.tmp" "${LOG_DIR}/x-route.log"
        fi
    fi
}

log_info() { log_msg "INFO" "$@"; }
log_warn() { log_msg "WARN" "$@"; }
log_err()  { log_msg "ERROR" "$@"; }

is_pid_running() {
    local pid_file="$1"
    if [ -f "${pid_file}" ]; then
        local pid
        pid="$(cat "${pid_file}" 2>/dev/null)"
        if [ -n "${pid}" ] && kill -0 "${pid}" 2>/dev/null; then
            return 0
        fi
    fi
    return 1
}

wait_until_boot_completed() {
    while [ "$(getprop sys.boot_completed)" != "1" ]; do
        sleep 2
    done
    # Wait up to 30s for FBE user 0 credential-encrypted storage if unlocked
    local retry=0
    while [ "$(getprop sys.user.0.ce_available)" != "true" ] && [ ! -d "/data/media/0" ] && [ "${retry}" -lt 15 ]; do
        sleep 2
        retry=$((retry + 1))
    done
}

acquire_wakelock() {
    if [ -w /sys/power/wake_lock ]; then
        echo "${WAKELOCK_NAME}" > /sys/power/wake_lock 2>/dev/null || true
    fi
}

release_wakelock() {
    if [ -w /sys/power/wake_unlock ]; then
        echo "${WAKELOCK_NAME}" > /sys/power/wake_unlock 2>/dev/null || true
    fi
}
